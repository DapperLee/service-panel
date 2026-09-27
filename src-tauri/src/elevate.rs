//! 管理员权限探测（Windows）。管理 Windows 服务需要管理员令牌；
//! 启动时探一次，缺权限就在 UI 横幅提示"以管理员重启"，而不是让用户对着 sc 的错误码 5 猜。
#![cfg(windows)]

use std::ffi::c_void;

type HANDLE = *mut c_void;
type BOOL = i32;
type DWORD = u32;
const TRUE: BOOL = 1;
const TOKEN_QUERY: DWORD = 0x0008;
#[allow(non_upper_case_globals)]
const TokenElevation: i32 = 20; // TOKEN_ELEVATION

#[repr(C)]
struct TokenElevation {
    token_is_filtered: DWORD,
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: HANDLE, access: DWORD, token: *mut HANDLE) -> BOOL;
    fn GetTokenInformation(
        token: HANDLE,
        class: i32,
        info: *mut c_void,
        length: DWORD,
        returned: *mut DWORD,
    ) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> HANDLE;
    fn CloseHandle(h: HANDLE) -> BOOL;
}

/// 当前进程是否以管理员（提升）令牌运行。
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != TRUE {
            return false;
        }
        let mut elev = TokenElevation {
            token_is_filtered: 0,
        };
        let mut ret: DWORD = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elev as *mut _ as *mut c_void,
            std::mem::size_of::<TokenElevation>() as DWORD,
            &mut ret,
        );
        CloseHandle(token);
        ok == TRUE && elev.token_is_filtered != 0
    }
}
