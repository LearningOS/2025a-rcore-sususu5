//! Process management syscalls
use core::mem::size_of;

use crate::mm::translated_byte_buffer;
use crate::task::{change_program_brk, check_address_readable, check_address_writable, current_mmap, current_munmap, current_user_token, exit_current_and_run_next, get_syscall_times, suspend_current_and_run_next};
use crate::timer::get_time_us;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    let us = get_time_us();
    let sec = us / 1000000;
    let usec = us % 1000000;
    
    let token = current_user_token();
    let buffers = translated_byte_buffer(
        token, ts as *const u8, size_of::<TimeVal>()
    );
    let timeval = TimeVal {sec, usec};
    let bytes = unsafe {
        core::slice::from_raw_parts(&timeval as *const TimeVal as *const u8, size_of::<TimeVal>())
    };
    let mut offset = 0;
    for buffer in buffers {
        let copy_len = buffer.len().min(bytes.len() - offset);
        buffer[..copy_len].copy_from_slice(&bytes[offset..offset + copy_len]);
        offset += copy_len;
    }
    0
}

/// TODO: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    match trace_request {
        0 => {
            if !check_address_readable(id) {
                return -1;
            } 
            let token =  current_user_token();
            let buffers = translated_byte_buffer(token, id as *const u8, 1);
            if buffers.is_empty() {
                return -1;
            }
            buffers[0][0] as isize
        }
        1 => {
            if !check_address_writable(id) {
                return -1;
            }
            let token = current_user_token();
            let mut buffers = translated_byte_buffer(token, id as *mut u8, 1);
            if buffers.is_empty() {
                return -1;
            }
            let byte_data = (data & 0xff) as u8;
            buffers[0][0] = byte_data;
            0
        }
        2 => {
            let times = get_syscall_times(id);
            times as isize
        }
        _ => {
            -1
        }
    }
}

// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    if current_mmap(start, len, prot).is_some() {
        0
    } else {
        -1
    }
}

// YOUR JOB: Implement munmap.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    if current_munmap(start, len).is_some() {
        0
    } else {
        -1
    }
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
