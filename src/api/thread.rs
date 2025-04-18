use core::alloc::Layout;

use alloc::sync::Arc;
use axhal::arch::TrapFrame;
use lock_api::{Mutex, RawMutex};

use crate::{
    DefaultSignalAction, PendingSignals, SignalAction, SignalActionFlags, SignalDisposition,
    SignalInfo, SignalOSAction, SignalSet, SignalStack, arch::UContext,
};

use super::{ProcessSignalManager, WaitQueue};

struct SignalFrame {
    ucontext: UContext,
    siginfo: SignalInfo,
    tf: TrapFrame,
}

/// Thread-level signal manager.
pub struct ThreadSignalManager<M, WQ> {
    /// The process-level signal manager
    proc: Arc<ProcessSignalManager<M, WQ>>,

    /// The pending signals
    pending: Mutex<M, PendingSignals>,
    /// The set of signals currently blocked from delivery.
    blocked: Mutex<M, SignalSet>,
    /// The stack used by signal handlers
    signal_stack: Mutex<M, SignalStack>,
}

impl<M: RawMutex, WQ: WaitQueue> ThreadSignalManager<M, WQ> {
    pub fn new(proc: Arc<ProcessSignalManager<M, WQ>>) -> Self {
        Self {
            proc,
            pending: Mutex::new(PendingSignals::new()),
            blocked: Mutex::new(SignalSet::default()),
            signal_stack: Mutex::new(SignalStack::default()),
        }
    }

    fn dequeue_signal(&self, mask: &SignalSet) -> Option<SignalInfo> {
        self.pending
            .lock()
            .dequeue_signal(mask)
            .or_else(|| self.proc.dequeue_signal(mask))
    }

    fn handle_signal(
        &self,
        tf: &mut TrapFrame,
        restore_blocked: SignalSet,
        sig: &SignalInfo,
        action: &SignalAction,
    ) -> Option<SignalOSAction> {
        let signo = sig.signo();
        info!("Handle signal: {:?} {}", signo, axtask::current().id_name());
        match action.disposition {
            SignalDisposition::Default => match signo.default_action() {
                DefaultSignalAction::Terminate => Some(SignalOSAction::Terminate),
                DefaultSignalAction::CoreDump => Some(SignalOSAction::CoreDump),
                DefaultSignalAction::Stop => Some(SignalOSAction::Stop),
                DefaultSignalAction::Ignore => None,
                DefaultSignalAction::Continue => Some(SignalOSAction::Continue),
            },
            SignalDisposition::Ignore => None,
            SignalDisposition::Handler(handler) => {
                let layout = Layout::new::<SignalFrame>();
                let stack = self.signal_stack.lock();
                let sp = if stack.disabled() || !action.flags.contains(SignalActionFlags::ONSTACK) {
                    tf.sp()
                } else {
                    stack.sp
                };
                drop(stack);

                // TODO: check if stack is large enough
                let aligned_sp = (sp - layout.size()) & !(layout.align() - 1);

                let frame_ptr = aligned_sp as *mut SignalFrame;
                // SAFETY: pointer is valid
                let frame = unsafe { &mut *frame_ptr };

                *frame = SignalFrame {
                    ucontext: UContext::new(tf, restore_blocked),
                    siginfo: sig.clone(),
                    tf: *tf,
                };

                tf.set_ip(handler as usize);
                tf.set_sp(aligned_sp);
                tf.set_arg0(signo as _);
                tf.set_arg1(&frame.siginfo as *const _ as _);
                tf.set_arg2(&frame.ucontext as *const _ as _);

                let restorer = action
                    .restorer
                    .map_or(self.proc.default_restorer, |f| f as _);
                #[cfg(target_arch = "x86_64")]
                tf.push_ra(restorer);
                #[cfg(not(target_arch = "x86_64"))]
                tf.set_ra(restorer);

                let mut add_blocked = action.mask;
                if !action.flags.contains(SignalActionFlags::NODEFER) {
                    add_blocked.add(signo);
                }

                if action.flags.contains(SignalActionFlags::RESETHAND) {
                    self.proc
                        .with_action_mut(signo, |a| *a = SignalAction::default());
                }
                *self.blocked.lock() |= add_blocked;
                Some(SignalOSAction::Handler)
            }
        }
    }

    /// Checks pending signals and handle them.
    ///
    /// Returns the signal number and the action the OS should take, if any.
    pub fn check_signals(
        &self,
        tf: &mut TrapFrame,
        restore_blocked: Option<SignalSet>,
    ) -> Option<(SignalInfo, SignalOSAction)> {
        let actions = self.proc.signal_actions.lock();

        let blocked = self.blocked.lock();
        let mask = !*blocked;
        let restore_blocked = restore_blocked.unwrap_or_else(|| *blocked);
        drop(blocked);

        loop {
            let Some(sig) = self.dequeue_signal(&mask) else {
                return None;
            };
            let action = &actions[sig.signo() as usize - 1];
            if let Some(os_action) = self.handle_signal(tf, restore_blocked, &sig, action) {
                break Some((sig, os_action));
            }
        }
    }

    /// Restores the signal frame. Called by `sigreutrn`.
    pub fn restore(&self, tf: &mut TrapFrame) {
        let frame_ptr = tf.sp() as *const SignalFrame;
        // SAFETY: pointer is valid
        let frame = unsafe { &*frame_ptr };

        *tf = frame.tf;
        frame.ucontext.mcontext.restore(tf);

        *self.blocked.lock() = frame.ucontext.sigmask;
    }

    /// Sends a signal to the thread.
    ///
    /// See [`ProcessSignalManager::send_signal`] for the process-level version.
    pub fn send_signal(&self, sig: SignalInfo) {
        self.pending.lock().put_signal(sig);
        self.proc.signal_wq.notify_all();
    }

    /// Applies a function to the blocked signals.
    pub fn with_blocked_mut<R>(&self, f: impl FnOnce(&mut SignalSet) -> R) -> R {
        f(&mut self.blocked.lock())
    }

    /// Gets current pending signals.
    pub fn pending(&self) -> SignalSet {
        self.pending.lock().set | self.proc.pending()
    }
}
