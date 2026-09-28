//! Windows Job Object 封装（KILL_ON_JOB_CLOSE）：句柄关闭时整棵进程树被系统回收。
//! 供 Command 驱动（托管进程）与日志尾随子进程（docker logs -f）复用，
//! 保证面板崩溃或被强杀时不会遗留孤儿进程。
#![cfg(windows)]
// 这些是 Win32 类型别名的原样拼写（HANDLE / BOOL），改名反而对不上 MSDN
#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::io;
use std::mem;
use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::ptr;

type HANDLE = *mut c_void;
type BOOL = i32;
const TRUE: BOOL = 1;

const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
#[allow(non_upper_case_globals)]
const JobObjectExtendedLimitInformation: i32 = 9;

#[repr(C)]
#[derive(Default)]
struct IoCounters {
    read_operations: u64,
    write_operations: u64,
    other_operations: u64,
    read_bytes: u64,
    write_bytes: u64,
    other_bytes: u64,
}

#[repr(C)]
#[derive(Default)]
struct BasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: *mut c_void,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
struct ExtendedLimitInformation {
    basic: BasicLimitInformation,
    io: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

impl Default for ExtendedLimitInformation {
    fn default() -> Self {
        unsafe { mem::zeroed() }
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(job_attributes: *const c_void, name: *const u16) -> HANDLE;
    fn SetInformationJobObject(
        job: HANDLE,
        job_object_information_class: i32,
        job_object_information: *const c_void,
        job_object_information_length: u32,
    ) -> BOOL;
    fn AssignProcessToJobObject(job: HANDLE, process: HANDLE) -> BOOL;
    fn CloseHandle(handle: HANDLE) -> BOOL;
    fn OpenProcess(desired_access: u32, inherit_handle: BOOL, process_id: u32) -> HANDLE;
}

/// 挂已有进程进 Job 所需的权限
const PROCESS_SET_QUOTA: u32 = 0x0100;
const PROCESS_TERMINATE: u32 = 0x0001;

pub struct Job {
    handle: HANDLE,
}
// HANDLE 是裸指针；Job 始终被持有者通过 Mutex 单点持有，手动声明 Send。
unsafe impl Send for Job {}

impl Job {
    pub fn new_kill_on_close() -> io::Result<Self> {
        unsafe {
            let handle = CreateJobObjectW(ptr::null(), ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut info = ExtendedLimitInformation::default();
            info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                mem::size_of::<ExtendedLimitInformation>() as u32,
            );
            if ok != TRUE {
                CloseHandle(handle);
                return Err(io::Error::last_os_error());
            }
            Ok(Job { handle })
        }
    }

    pub fn assign(&self, child: &Child) -> io::Result<()> {
        unsafe {
            let ok = AssignProcessToJobObject(self.handle, child.as_raw_handle() as HANDLE);
            if ok == TRUE {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    /// 把**已经在运行**的进程挂进本 Job（面板重启后接管场景）。
    /// 挂不上就返回 Err，调用方据此放弃接管——绝不在没有 Job 兜底的情况下认领进程。
    pub fn assign_pid(&self, pid: u32) -> io::Result<()> {
        unsafe {
            let h = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if h.is_null() {
                return Err(io::Error::last_os_error());
            }
            let ok = AssignProcessToJobObject(self.handle, h);
            CloseHandle(h);
            if ok == TRUE {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
