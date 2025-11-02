//! Implementation of  [`ProcessControlBlock`]

use super::id::RecycleAllocator;
use super::manager::insert_into_pid2process;
use super::TaskControlBlock;
use super::{add_task, SignalFlags};
use super::{pid_alloc, PidHandle};
use crate::fs::{File, Stdin, Stdout};
use crate::mm::{translated_refmut, MemorySet, KERNEL_SPACE};
use crate::sync::{Condvar, Mutex, Semaphore, UPSafeCell};
use crate::trap::{trap_handler, TrapContext};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefMut;

/// Process Control Block
pub struct ProcessControlBlock {
    /// immutable
    pub pid: PidHandle,
    /// mutable
    inner: UPSafeCell<ProcessControlBlockInner>,
}

/// Inner of Process Control Block
pub struct ProcessControlBlockInner {
    /// is zombie?
    pub is_zombie: bool,
    /// memory set(address space)
    pub memory_set: MemorySet,
    /// parent process
    pub parent: Option<Weak<ProcessControlBlock>>,
    /// children process
    pub children: Vec<Arc<ProcessControlBlock>>,
    /// exit code
    pub exit_code: i32,
    /// file descriptor table
    pub fd_table: Vec<Option<Arc<dyn File + Send + Sync>>>,
    /// signal flags
    pub signals: SignalFlags,
    /// tasks(also known as threads)
    pub tasks: Vec<Option<Arc<TaskControlBlock>>>,
    /// task resource allocator
    pub task_res_allocator: RecycleAllocator,
    /// mutex list
    pub mutex_list: Vec<Option<Arc<dyn Mutex>>>,
    /// semaphore list
    pub semaphore_list: Vec<Option<Arc<Semaphore>>>,
    /// condvar list
    pub condvar_list: Vec<Option<Arc<Condvar>>>,
    /// available mutex resources
    pub m_available: Vec<usize>,
    /// available semaphore resources
    pub s_available: Vec<usize>,
    /// whether deadlock detection is enabled
    pub use_dead_lock: bool,
}

impl ProcessControlBlockInner {
    #[allow(unused)]
    /// get the address of app's page table
    pub fn get_user_token(&self) -> usize {
        self.memory_set.token()
    }
    /// allocate a new file descriptor
    pub fn alloc_fd(&mut self) -> usize {
        if let Some(fd) = (0..self.fd_table.len()).find(|fd| self.fd_table[*fd].is_none()) {
            fd
        } else {
            self.fd_table.push(None);
            self.fd_table.len() - 1
        }
    }
    /// allocate a new task id
    pub fn alloc_tid(&mut self) -> usize {
        self.task_res_allocator.alloc()
    }
    /// deallocate a task id
    pub fn dealloc_tid(&mut self, tid: usize) {
        self.task_res_allocator.dealloc(tid)
    }
    /// the count of tasks(threads) in this process
    pub fn thread_count(&self) -> usize {
        self.tasks.len()
    }
    /// get a task with tid in this process
    pub fn get_task(&self, tid: usize) -> Arc<TaskControlBlock> {
        self.tasks[tid].as_ref().unwrap().clone()
    }
}

impl ProcessControlBlock {
    /// inner_exclusive_access
    pub fn inner_exclusive_access(&self) -> RefMut<'_, ProcessControlBlockInner> {
        self.inner.exclusive_access()
    }
    /// new process from elf file
    pub fn new(elf_data: &[u8]) -> Arc<Self> {
        trace!("kernel: ProcessControlBlock::new");
        // memory_set with elf program headers/trampoline/trap context/user stack
        let (memory_set, ustack_base, entry_point) = MemorySet::from_elf(elf_data);
        // allocate a pid
        let pid_handle = pid_alloc();
        let process = Arc::new(Self {
            pid: pid_handle,
            inner: unsafe {
                UPSafeCell::new(ProcessControlBlockInner {
                    is_zombie: false,
                    memory_set,
                    parent: None,
                    children: Vec::new(),
                    exit_code: 0,
                    fd_table: vec![
                        // 0 -> stdin
                        Some(Arc::new(Stdin)),
                        // 1 -> stdout
                        Some(Arc::new(Stdout)),
                        // 2 -> stderr
                        Some(Arc::new(Stdout)),
                    ],
                    signals: SignalFlags::empty(),
                    tasks: Vec::new(),
                    task_res_allocator: RecycleAllocator::new(),
                    mutex_list: Vec::new(),
                    semaphore_list: Vec::new(),
                    condvar_list: Vec::new(),
                    m_available: Vec::new(),
                    s_available: Vec::new(),
                    use_dead_lock: false,
                })
            },
        });
        // create a main thread, we should allocate ustack and trap_cx here
        let task = Arc::new(TaskControlBlock::new(
            Arc::clone(&process),
            ustack_base,
            true,
        ));
        // prepare trap_cx of main thread
        let task_inner = task.inner_exclusive_access();
        let trap_cx = task_inner.get_trap_cx();
        let ustack_top = task_inner.res.as_ref().unwrap().ustack_top();
        let kstack_top = task.kstack.get_top();
        drop(task_inner);
        *trap_cx = TrapContext::app_init_context(
            entry_point,
            ustack_top,
            KERNEL_SPACE.exclusive_access().token(),
            kstack_top,
            trap_handler as usize,
        );
        // add main thread to the process
        let mut process_inner = process.inner_exclusive_access();
        process_inner.tasks.push(Some(Arc::clone(&task)));
        drop(process_inner);
        insert_into_pid2process(process.getpid(), Arc::clone(&process));
        // add main thread to scheduler
        add_task(task);
        process
    }
    /// Only support processes with a single thread.
    pub fn exec(self: &Arc<Self>, elf_data: &[u8], args: Vec<String>) {
        trace!("kernel: exec");
        assert_eq!(self.inner_exclusive_access().thread_count(), 1);
        // memory_set with elf program headers/trampoline/trap context/user stack
        trace!("kernel: exec .. MemorySet::from_elf");
        let (memory_set, ustack_base, entry_point) = MemorySet::from_elf(elf_data);
        let new_token = memory_set.token();
        // substitute memory_set
        trace!("kernel: exec .. substitute memory_set");
        self.inner_exclusive_access().memory_set = memory_set;
        // then we alloc user resource for main thread again
        // since memory_set has been changed
        trace!("kernel: exec .. alloc user resource for main thread again");
        let task = self.inner_exclusive_access().get_task(0);
        let mut task_inner = task.inner_exclusive_access();
        task_inner.res.as_mut().unwrap().ustack_base = ustack_base;
        task_inner.res.as_mut().unwrap().alloc_user_res();
        task_inner.trap_cx_ppn = task_inner.res.as_mut().unwrap().trap_cx_ppn();
        // push arguments on user stack
        trace!("kernel: exec .. push arguments on user stack");
        let mut user_sp = task_inner.res.as_mut().unwrap().ustack_top();
        user_sp -= (args.len() + 1) * core::mem::size_of::<usize>();
        let argv_base = user_sp;
        let mut argv: Vec<_> = (0..=args.len())
            .map(|arg| {
                translated_refmut(
                    new_token,
                    (argv_base + arg * core::mem::size_of::<usize>()) as *mut usize,
                )
            })
            .collect();
        *argv[args.len()] = 0;
        for i in 0..args.len() {
            user_sp -= args[i].len() + 1;
            *argv[i] = user_sp;
            let mut p = user_sp;
            for c in args[i].as_bytes() {
                *translated_refmut(new_token, p as *mut u8) = *c;
                p += 1;
            }
            *translated_refmut(new_token, p as *mut u8) = 0;
        }
        // make the user_sp aligned to 8B for k210 platform
        user_sp -= user_sp % core::mem::size_of::<usize>();
        // initialize trap_cx
        trace!("kernel: exec .. initialize trap_cx");
        let mut trap_cx = TrapContext::app_init_context(
            entry_point,
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            task.kstack.get_top(),
            trap_handler as usize,
        );
        trap_cx.x[10] = args.len();
        trap_cx.x[11] = argv_base;
        *task_inner.get_trap_cx() = trap_cx;
    }

    /// Only support processes with a single thread.
    pub fn fork(self: &Arc<Self>) -> Arc<Self> {
        trace!("kernel: fork");
        let mut parent = self.inner_exclusive_access();
        assert_eq!(parent.thread_count(), 1);
        // clone parent's memory_set completely including trampoline/ustacks/trap_cxs
        let memory_set = MemorySet::from_existed_user(&parent.memory_set);
        // alloc a pid
        let pid = pid_alloc();
        // copy fd table
        let mut new_fd_table: Vec<Option<Arc<dyn File + Send + Sync>>> = Vec::new();
        for fd in parent.fd_table.iter() {
            if let Some(file) = fd {
                new_fd_table.push(Some(file.clone()));
            } else {
                new_fd_table.push(None);
            }
        }
        // create child process pcb
        let child = Arc::new(Self {
            pid,
            inner: unsafe {
                UPSafeCell::new(ProcessControlBlockInner {
                    is_zombie: false,
                    memory_set,
                    parent: Some(Arc::downgrade(self)),
                    children: Vec::new(),
                    exit_code: 0,
                    fd_table: new_fd_table,
                    signals: SignalFlags::empty(),
                    tasks: Vec::new(),
                    task_res_allocator: RecycleAllocator::new(),
                    mutex_list: Vec::new(),
                    semaphore_list: Vec::new(),
                    condvar_list: Vec::new(),
                    m_available: Vec::new(),
                    s_available: Vec::new(),
                    use_dead_lock: false,
                })
            },
        });
        // add child
        parent.children.push(Arc::clone(&child));
        // create main thread of child process
        let task = Arc::new(TaskControlBlock::new(
            Arc::clone(&child),
            parent
                .get_task(0)
                .inner_exclusive_access()
                .res
                .as_ref()
                .unwrap()
                .ustack_base(),
            // here we do not allocate trap_cx or ustack again
            // but mention that we allocate a new kstack here
            false,
        ));
        // attach task to child process
        let mut child_inner = child.inner_exclusive_access();
        child_inner.tasks.push(Some(Arc::clone(&task)));
        drop(child_inner);
        // modify kstack_top in trap_cx of this thread
        let task_inner = task.inner_exclusive_access();
        let trap_cx = task_inner.get_trap_cx();
        trap_cx.kernel_sp = task.kstack.get_top();
        drop(task_inner);
        insert_into_pid2process(child.getpid(), Arc::clone(&child));
        // add this thread to scheduler
        add_task(task);
        child
    }
    /// get pid
    pub fn getpid(&self) -> usize {
        self.pid.0
    }
}

impl ProcessControlBlockInner {
    pub fn expand_mutex_slot(&mut self, id: usize) {
        if self.m_available.len() <= id {
            self.m_available.resize(id + 1, 0);
        }
        for task in self.tasks.iter().filter_map(|t| t.as_ref()) {
            let mut task_inner = task.inner_exclusive_access();
            if task_inner.m_allocation.len() <= id {
                task_inner.m_allocation.resize(id + 1, 0);
            }
            if task_inner.m_need.len() <= id {
                task_inner.m_need.resize(id + 1, 0);
            }
        }
    }

    pub fn expand_semaphore_slot(&mut self, id: usize) {
        if self.s_available.len() <= id {
            self.s_available.resize(id + 1, 0);
        }
        for task in self.tasks.iter().filter_map(|t| t.as_ref()) {
            let mut task_inner = task.inner_exclusive_access();
            if task_inner.s_allocation.len() <= id {
                task_inner.s_allocation.resize(id + 1, 0);
            }
            if task_inner.s_need.len() <= id {
                task_inner.s_need.resize(id + 1, 0);
            }
        }
    }

    pub fn is_mutex_state_safe(&self) -> bool {
        let states = self.collect_mutex_states();
        is_state_safe(&self.m_available, states)
    }

    pub fn is_semaphore_state_safe(&self) -> bool {
        let states = self.collect_semaphore_states();
        is_state_safe(&self.s_available, states)
    }

    fn collect_mutex_states(&self) -> Vec<Option<(Vec<usize>, Vec<usize>)>> {
        let len = self.m_available.len();
        self.tasks
            .iter()
            .map(|task_opt| {
                task_opt.as_ref().map(|task| {
                    let mut inner = task.inner_exclusive_access();
                    if inner.m_allocation.len() < len {
                        inner.m_allocation.resize(len, 0);
                    }
                    if inner.m_need.len() < len {
                        inner.m_need.resize(len, 0);
                    }
                    let allocation = inner.m_allocation.clone();
                    let need = inner.m_need.clone();
                    (allocation, need)})
            })
            .collect()
    }

    fn collect_semaphore_states(&self) -> Vec<Option<(Vec<usize>, Vec<usize>)>> {
        let len = self.s_available.len();
        self.tasks
            .iter()
            .map(|task_opt| {
                task_opt.as_ref().map(|task| {
                    let mut inner = task.inner_exclusive_access();
                    if inner.s_allocation.len() < len {
                        inner.s_allocation.resize(len, 0);
                    }
                    if inner.s_need.len() < len {
                        inner.s_need.resize(len, 0);
                    }
                    let allocation = inner.s_allocation.clone();
                    let need = inner.s_need.clone();

                    (allocation, need)})
            })
            .collect()
    }
}

fn is_state_safe(available: &[usize], states: Vec<Option<(Vec<usize>, Vec<usize>)>>) -> bool {
    let mut work = available.to_vec();
    let mut resource_len = work.len();
    for state in states.iter() {
        if let Some((alloc, need)) = state {
            resource_len = resource_len.max(alloc.len());
            resource_len = resource_len.max(need.len());
        }
    }
    work.resize(resource_len, 0);

    let mut allocations = Vec::with_capacity(states.len());
    let mut needs = Vec::with_capacity(states.len());
    let mut finish = Vec::with_capacity(states.len());

    for state in states.into_iter() {
        match state {
            Some((mut alloc, mut need)) => {
                if alloc.len() < resource_len {
                    alloc.resize(resource_len, 0);
                }
                if need.len() < resource_len {
                    need.resize(resource_len, 0);
                }
                allocations.push(alloc);
                needs.push(need);
                finish.push(false);
            }
            None => {
                allocations.push(vec![0; resource_len]);
                needs.push(vec![0; resource_len]);
                finish.push(true);
            }
        }
    }

    loop {
        let mut progressed = false;
        for i in 0..allocations.len() {
            if finish[i] {
                continue;
            }
            let can_finish = (0..resource_len).all(|j| needs[i][j] <= work[j]);
            if can_finish {
                for j in 0..resource_len {
                    work[j] = work[j].saturating_add(allocations[i][j]);
                }
                finish[i] = true;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    finish.into_iter().all(|done| done)
}
