use crate::sync::{Condvar, Mutex, MutexBlocking, MutexSpin, Semaphore};
use crate::task::{block_current_and_run_next, current_process, current_task};
use crate::timer::{add_timer, get_time_ms};
use alloc::sync::Arc;

const DEADLOCK_ERR: isize = -(0xDEAD as isize);
/// sleep syscall
pub fn sys_sleep(ms: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_sleep",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let expire_ms = get_time_ms() + ms;
    let task = current_task().unwrap();
    add_timer(expire_ms, task);
    block_current_and_run_next();
    0
}

/// mutex create syscall
pub fn sys_mutex_create(blocking: bool) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mutex: Option<Arc<dyn Mutex>> = if !blocking {
        Some(Arc::new(MutexSpin::new()))
    } else {
        Some(Arc::new(MutexBlocking::new()))
    };
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .mutex_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.mutex_list[id] = mutex;
        id
    } else {
        process_inner.mutex_list.push(mutex);
        process_inner.mutex_list.len() - 1
    };
    process_inner.expand_mutex_slot(id);
    process_inner.m_available[id] = 1;
    id as isize
}

/// mutex lock syscall
pub fn sys_mutex_lock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_lock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task().unwrap().inner_exclusive_access().res.as_ref().unwrap().tid
    );
    let task = current_task().unwrap();
    let process = current_process();
    let mutex = {
        let mut inner = process.inner_exclusive_access();
        let mutex_entry = match inner.mutex_list.get(mutex_id).and_then(|slot| slot.as_ref()) {
            Some(m) => Arc::clone(m),
            None => return -1,
        };
        inner.expand_mutex_slot(mutex_id);
        {
            let mut t = task.inner_exclusive_access();
            if t.m_need.len() <= mutex_id { t.m_need.resize(mutex_id + 1, 0); }
            if t.m_allocation.len() <= mutex_id { t.m_allocation.resize(mutex_id + 1, 0); }
            t.m_need[mutex_id] += 1;
        }
        if inner.use_dead_lock && !inner.is_mutex_state_safe() {
            let mut t = task.inner_exclusive_access();
            if t.m_need.len() > mutex_id && t.m_need[mutex_id] > 0 { t.m_need[mutex_id] -= 1; }
            return DEADLOCK_ERR;
        }
        mutex_entry
    };

    mutex.lock();

    let mut inner = process.inner_exclusive_access();
    if inner.m_available.len() <= mutex_id { inner.m_available.resize(mutex_id + 1, 0); }
    if inner.m_available[mutex_id] > 0 { inner.m_available[mutex_id] -= 1; }
    {
        let mut t = task.inner_exclusive_access();
        if t.m_need.len() <= mutex_id { t.m_need.resize(mutex_id + 1, 0); }
        if t.m_need[mutex_id] > 0 { t.m_need[mutex_id] -= 1; }
        if t.m_allocation.len() <= mutex_id { t.m_allocation.resize(mutex_id + 1, 0); }
        t.m_allocation[mutex_id] += 1;
    }
    0
}

/// mutex unlock syscall
pub fn sys_mutex_unlock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_unlock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task().unwrap().inner_exclusive_access().res.as_ref().unwrap().tid
    );
    let task = current_task().unwrap();
    let process = current_process();

    let mutex = {
        let inner = process.inner_exclusive_access();
        match inner.mutex_list.get(mutex_id).and_then(|slot| slot.as_ref()) {
            Some(m) => Arc::clone(m),
            None => return -1,
        }
    };

    drop(process);
    mutex.unlock();

    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    inner.expand_mutex_slot(mutex_id);
    if inner.m_available.len() <= mutex_id { inner.m_available.resize(mutex_id + 1, 0); }
    inner.m_available[mutex_id] = 1;

    {
        let mut t = task.inner_exclusive_access();
        if t.m_allocation.len() <= mutex_id { t.m_allocation.resize(mutex_id + 1, 0); }
        if t.m_allocation[mutex_id] > 0 { t.m_allocation[mutex_id] -= 1; }
        if t.m_need.len() <= mutex_id { t.m_need.resize(mutex_id + 1, 0); }
        t.m_need[mutex_id] = 0;
    }
    0
}


/// semaphore create syscall
pub fn sys_semaphore_create(res_count: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .semaphore_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.semaphore_list[id] = Some(Arc::new(Semaphore::new(res_count)));
        id
    } else {
        process_inner
            .semaphore_list
            .push(Some(Arc::new(Semaphore::new(res_count))));
        process_inner.semaphore_list.len() - 1
    };
    process_inner.expand_semaphore_slot(id);
    if process_inner.s_available.len() <= id {
        process_inner.s_available.resize(id + 1, 0);
    }
    process_inner.s_available[id] = res_count;
    id as isize
}

/// semaphore up syscall
pub fn sys_semaphore_up(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_up",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );

    let task = current_task().unwrap();
    let process = current_process();

    let sem = {
        let mut inner = process.inner_exclusive_access();
        let sem_entry = match inner.semaphore_list.get(sem_id).and_then(|slot| slot.as_ref()) {
            Some(s) => Arc::clone(s),
            None => return -1,
        };
        inner.expand_semaphore_slot(sem_id);
        sem_entry
    };

    drop(process);
    sem.up();

    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    inner.expand_semaphore_slot(sem_id);

    {
        let mut t = task.inner_exclusive_access();
        if t.s_allocation.len() <= sem_id {
            t.s_allocation.resize(sem_id + 1, 0);
        }
        if t.s_allocation[sem_id] == 0 {
            return -1;
        }
        t.s_allocation[sem_id] -= 1;
    }

    if inner.s_available.len() <= sem_id {
        inner.s_available.resize(sem_id + 1, 0);
    }
    inner.s_available[sem_id] = inner.s_available[sem_id].saturating_add(1);

    0
}


/// semaphore down syscall
pub fn sys_semaphore_down(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_down",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let task = current_task().unwrap();
    let process = current_process();

    let sem = {
        let mut inner = process.inner_exclusive_access();
        let sem_entry = match inner.semaphore_list.get(sem_id).and_then(|slot| slot.as_ref()) {
            Some(s) => Arc::clone(s),
            None => return -1,
        };
        inner.expand_semaphore_slot(sem_id);

        {
            let mut t = task.inner_exclusive_access();
            if t.s_need.len() <= sem_id { t.s_need.resize(sem_id + 1, 0); }
            if t.s_allocation.len() <= sem_id { t.s_allocation.resize(sem_id + 1, 0); }
            t.s_need[sem_id] += 1;
        }

        if inner.use_dead_lock && !inner.is_semaphore_state_safe() {
            let mut t = task.inner_exclusive_access();
            if t.s_need.len() > sem_id && t.s_need[sem_id] > 0 { t.s_need[sem_id] -= 1; }
            return DEADLOCK_ERR;
        }
        sem_entry
    };

    sem.down();

    let mut inner = process.inner_exclusive_access();
    inner.expand_semaphore_slot(sem_id);
    if inner.s_available.len() <= sem_id { inner.s_available.resize(sem_id + 1, 0); }
    inner.s_available[sem_id] = inner.s_available[sem_id].saturating_sub(1);

    {
        let mut t = task.inner_exclusive_access();
        if t.s_need.len() <= sem_id { t.s_need.resize(sem_id + 1, 0); }
        if let Some(slot) = t.s_need.get_mut(sem_id) { *slot = slot.saturating_sub(1); }
        if t.s_allocation.len() <= sem_id { t.s_allocation.resize(sem_id + 1, 0); }
        t.s_allocation[sem_id] += 1;
    }
    0
}

/// condvar create syscall
pub fn sys_condvar_create() -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .condvar_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.condvar_list[id] = Some(Arc::new(Condvar::new()));
        id
    } else {
        process_inner
            .condvar_list
            .push(Some(Arc::new(Condvar::new())));
        process_inner.condvar_list.len() - 1
    };
    id as isize
}
/// condvar signal syscall
pub fn sys_condvar_signal(condvar_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_signal",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    drop(process_inner);
    condvar.signal();
    0
}
/// condvar wait syscall
pub fn sys_condvar_wait(condvar_id: usize, mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_wait",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    condvar.wait(mutex);
    0
}

/// enable deadlock detection syscall
///
/// YOUR JOB: Implement deadlock detection, but might not all in this syscall
pub fn sys_enable_deadlock_detect(enabled: usize) -> isize {
    if enabled > 1 {
        return -1;
    }
    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    if enabled == 0 {
        inner.use_dead_lock = false;
        return 0;
    }
    if !inner.is_mutex_state_safe() {
        return -1;
    }
    if !inner.is_semaphore_state_safe() {
        return -1;
    }
    inner.use_dead_lock = true;
    0
}
