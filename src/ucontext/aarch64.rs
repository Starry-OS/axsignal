use axhal::arch::{GeneralRegisters, TrapFrame};
use linux_raw_sys::general::SS_DISABLE;

use crate::ctypes::SignalSet;

#[repr(C)]
#[derive(Clone)]
pub struct SignalStack {
    pub sp: usize,
    pub flags: u32,
    pub size: usize,
}

impl Default for SignalStack {
    fn default() -> Self {
        Self {
            sp: 0,
            flags: SS_DISABLE,
            size: 0,
        }
    }
}

#[repr(C, align(16))]
struct MContextPadding([u8; 4096]);

#[repr(C)]
#[derive(Clone)]
pub struct MContext {
    fault_address: u64,
    regs: [u64; 31],
    sp: u64,
    pc: u64,
    pstate: u64,
    __reserved: MContextPadding,
}

impl MContext {
    pub fn new(tf: &TrapFrame) -> Self {
        Self {
            fault_address: 0,
            regs: tf.r,
            sp: tf.usp,
            pc: tf.elr,
            pstate: tf.spsr,
            __reserved: MContextPadding([0; 4096]),
        }
    }

    pub fn restore(&self, tf: &mut TrapFrame) {
        tf.r = self.regs;
        tf.usp = self.sp;
        tf.elr = self.pc;
        tf.spsr = self.pstate;
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct UContext {
    pub flags: usize,
    pub link: usize,
    pub stack: SignalStack,
    pub sigmask: SignalSet,
    __unused: [u8; 1024 / 8 - size_of::<SignalSet>()],
    pub mcontext: MContext,
}

impl UContext {
    pub fn new(tf: &TrapFrame, sigmask: SignalSet) -> Self {
        Self {
            flags: 0,
            link: 0,
            stack: SignalStack::default(),
            sigmask,
            __unused: [0; 1024 / 8 - size_of::<SignalSet>()],
            mcontext: MContext::new(tf),
        }
    }
}
