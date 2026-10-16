//! 内存探针：只在 `cfg(test)` 编译，生产的 dll 里不带这一块。
//!
//! 这一轮要回答的问题是"字体那一份轮廓常驻，出图结束之后还回不还得掉"，
//! 而任务管理器只给一个进程总数。这里用一层计数分配器包住系统分配器，得到三个数：
//! 存活字节、高水位、以及 drop 之后有没有回到基线；另外从 `K32GetProcessMemoryInfo`
//! 取工作集与私有字节——任务管理器「内存」那一列就是工作集，光看存活字节回答不了
//! "还不还给系统"这一个问题。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Counting;

/// 记一笔：`old` → `new`
fn track(old: usize, new: usize) {
    // 用 wrapping：计数只是观测，不该因为一次加减顺序问题把测试弄崩
    let live = if new >= old {
        LIVE.fetch_add(new - old, Ordering::Relaxed) + (new - old)
    } else {
        LIVE.fetch_sub(old - new, Ordering::Relaxed)
            .saturating_sub(old - new)
    };
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            track(0, layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            track(0, layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        track(layout.size(), 0);
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let done = unsafe { System.realloc(ptr, layout, new_size) };
        if !done.is_null() {
            track(layout.size(), new_size);
        }
        done
    }
}

/// 当前存活字节
pub(crate) fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// 打印成"多少 MB"
pub(crate) fn mb(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / 1048576.0)
}

/// 操作系统那一头的读数：（工作集, 峰值工作集, 私有字节）。非 Windows 回 0。
pub(crate) fn os_footprint() -> (usize, usize, usize) {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        // PROCESS_MEMORY_COUNTERS_EX 的字段一个不能少：`cb` 报的长度对不上，API 直接回 FALSE
        #[repr(C)]
        struct Counters {
            cb: u32,
            page_faults: u32,
            peak_working_set: usize,
            working_set: usize,
            quota_peak_paged: usize,
            quota_paged: usize,
            quota_peak_nonpaged: usize,
            quota_nonpaged: usize,
            pagefile: usize,
            peak_pagefile: usize,
            private_usage: usize,
        }
        unsafe extern "system" {
            fn K32GetProcessMemoryInfo(
                process: *mut c_void,
                counters: *mut Counters,
                cb: u32,
            ) -> i32;
        }
        let mut taken = Counters {
            cb: std::mem::size_of::<Counters>() as u32,
            page_faults: 0,
            peak_working_set: 0,
            working_set: 0,
            quota_peak_paged: 0,
            quota_paged: 0,
            quota_peak_nonpaged: 0,
            quota_nonpaged: 0,
            pagefile: 0,
            peak_pagefile: 0,
            private_usage: 0,
        };
        // (HANDLE)-1 就是当前进程
        let current = -1isize as *mut c_void;
        let ok = unsafe { K32GetProcessMemoryInfo(current, &mut taken, taken.cb) };
        if ok == 0 {
            return (0, 0, 0);
        }
        (
            taken.working_set,
            taken.peak_working_set,
            taken.private_usage,
        )
    }
    #[cfg(not(windows))]
    {
        (0, 0, 0)
    }
}
