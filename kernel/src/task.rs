//! Preemptive multitasking.
//!
//! Each task has its own stack. About 100 times a second the timer interrupt
//! fires; its handler saves every CPU register of the running task onto that
//! task's stack, asks the scheduler which task runs next, switches to that
//! task's stack, and restores its registers. To each task it looks as if it
//! has the CPU to itself.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::naked_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::segmentation::{CS, SS, Segment};

const STACK_SIZE: usize = 64 * 1024;
pub const TICKS_PER_SECOND: u64 = 100;

static TICKS: AtomicU64 = AtomicU64::new(0);
static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler { tasks: Vec::new(), current: 0 });

#[derive(PartialEq)]
enum State {
    Ready,
    Sleeping { until: u64 },
    Finished,
}

struct Task {
    name: &'static str,
    /// Saved stack pointer while the task isn't running.
    rsp: u64,
    state: State,
    /// The task's stack memory, kept here so it isn't freed while in use.
    /// `None` for the boot task, which runs on the bootloader's stack.
    _stack: Option<Box<[u8]>>,
    entry: Option<fn()>,
}

struct Scheduler {
    tasks: Vec<Task>,
    current: usize,
}

/// Turn the code that's running right now into task 0 ("kernel").
pub fn init() {
    without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        sched.tasks.reserve(16);
        sched.tasks.push(Task { name: "kernel", rsp: 0, state: State::Ready, _stack: None, entry: None });
    });
}

/// Start running `entry` as a new task alongside everything else.
pub fn spawn(name: &'static str, entry: fn()) {
    let mut stack = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
    let top = (stack.as_mut_ptr() as u64 + STACK_SIZE as u64) & !0xF;

    // Build the stack exactly as the timer handler leaves it, so the first
    // switch to this task "returns" into `task_start`.
    let frame = [
        SS::get_reg().0 as u64,
        top - 8, // the stack pointer the task starts with
        0x202,   // RFLAGS with interrupts enabled
        CS::get_reg().0 as u64,
        task_start as *const () as u64,
    ];
    let mut rsp = top;
    unsafe {
        for value in frame {
            rsp -= 8;
            *(rsp as *mut u64) = value;
        }
        // 15 general-purpose registers, all starting at zero.
        for _ in 0..15 {
            rsp -= 8;
            *(rsp as *mut u64) = 0;
        }
    }

    without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        // Reuse a finished task's slot so the list doesn't grow forever.
        let task = Task { name, rsp, state: State::Ready, _stack: Some(stack), entry: Some(entry) };
        if let Some(slot) = sched.tasks.iter().position(|t| t.state == State::Finished) {
            sched.tasks[slot] = task;
        } else {
            sched.tasks.push(task);
        }
    });
}

/// Every new task begins here.
extern "C" fn task_start() -> ! {
    let entry = without_interrupts(|| {
        let sched = SCHEDULER.lock();
        sched.tasks[sched.current].entry.expect("task has no entry point")
    });
    entry();
    without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let current = sched.current;
        sched.tasks[current].state = State::Finished;
    });
    loop {
        x86_64::instructions::hlt();
    }
}

/// Pause the current task for at least `ms` milliseconds.
pub fn sleep_ms(ms: u64) {
    let until = ticks() + ms.div_ceil(1000 / TICKS_PER_SECOND);
    without_interrupts(|| {
        let mut sched = SCHEDULER.lock();
        let current = sched.current;
        sched.tasks[current].state = State::Sleeping { until };
    });
    while ticks() < until {
        x86_64::instructions::hlt();
    }
}

/// Timer ticks since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Names of tasks that are still alive, for display.
pub fn list(mut f: impl FnMut(&str)) {
    without_interrupts(|| {
        for task in SCHEDULER.lock().tasks.iter().filter(|t| t.state != State::Finished) {
            f(task.name);
        }
    });
}

/// Called by the timer handler with the interrupted task's stack pointer.
/// Returns the stack pointer of the task to run next. Must never allocate:
/// the interrupted task might be halfway through an allocation itself.
extern "C" fn schedule(rsp: u64) -> u64 {
    let now = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    crate::interrupts::end_of_timer_interrupt();

    // `try_lock` because the interrupted code might hold the lock; if so,
    // just keep running it and switch on the next tick.
    let Some(mut sched) = SCHEDULER.try_lock() else { return rsp };
    if sched.tasks.is_empty() {
        return rsp;
    }
    let current = sched.current;
    sched.tasks[current].rsp = rsp;

    let count = sched.tasks.len();
    for step in 1..=count {
        let index = (current + step) % count;
        let task = &mut sched.tasks[index];
        if let State::Sleeping { until } = task.state {
            if now >= until {
                task.state = State::Ready;
            }
        }
        if task.state == State::Ready || index == 0 {
            // Task 0 (the kernel's idle loop) can always run.
            sched.current = index;
            return sched.tasks[index].rsp;
        }
    }
    rsp
}

/// The timer interrupt entry point. Written in assembly because it has to
/// save and restore registers by hand to swap stacks.
#[unsafe(naked)]
pub extern "C" fn timer_interrupt_entry() {
    naked_asm!(
        "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
        "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
        "mov rdi, rsp",
        "call {schedule}",
        "mov rsp, rax",
        "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
        "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
        "iretq",
        schedule = sym schedule,
    );
}
