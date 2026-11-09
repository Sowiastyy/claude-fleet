//! Tying the voice engines' lives to the fleet's.
//!
//! The recogniser holds most of a gigabyte of GPU memory for as long as it
//! runs. Killing it on the way out covers a clean quit; a fleet that crashes
//! or is killed never gets that far. A job object set to kill its processes
//! when its last handle closes covers that case too: the handle is the
//! fleet's, and Windows closes it whatever way the fleet goes.

use std::process::Child;

#[cfg(windows)]
mod imp {
    use std::{
        ffi::c_void,
        os::windows::io::AsRawHandle,
        process::Child,
        sync::OnceLock,
    };

    type Handle = *mut c_void;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        counts: [u64; 6],
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *mut c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    }

    /// The handle as an integer, so it can live in a static.
    static JOB: OnceLock<Option<usize>> = OnceLock::new();

    fn job() -> Option<Handle> {
        let raw = JOB.get_or_init(|| {
            // SAFETY: plain Win32 calls with a zeroed, correctly sized struct.
            // The handle is never closed: closing it is what the fleet's exit does.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut info = ExtendedLimits::default();
                info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                    (&mut info as *mut ExtendedLimits).cast(),
                    std::mem::size_of::<ExtendedLimits>() as u32,
                );
                (ok != 0).then_some(job as usize)
            }
        });
        raw.map(|h| h as Handle)
    }

    pub fn adopt(child: &Child) -> bool {
        let Some(job) = job() else {
            return false;
        };
        // SAFETY: the process handle is valid for as long as `child` is.
        unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) != 0 }
    }
}

/// Put an engine process in the job that dies with the fleet. Returns
/// whether it took; when it did not, the caller's own kill on drop is all
/// there is.
pub fn adopt(child: &Child) -> bool {
    #[cfg(windows)]
    {
        imp::adopt(child)
    }
    #[cfg(not(windows))]
    {
        let _ = child;
        false
    }
}
