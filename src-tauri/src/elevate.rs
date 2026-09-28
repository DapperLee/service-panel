//! 管理员权限探测（Windows）。管理 Windows 服务需要管理员令牌；
//! 启动时探一次，缺权限就在 UI 横幅提示"以管理员重启"，而不是让用户对着 sc 的错误码 5 猜。
#![cfg(windows)]
// 下面这些是 Win32 类型别名的原样拼写（HANDLE / BOOL / DWORD），改名反而对不上文档
#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;

type HANDLE = *mut c_void;
type BOOL = i32;
type DWORD = u32;
const TRUE: BOOL = 1;
const TOKEN_QUERY: DWORD = 0x0008;
#[allow(non_upper_case_globals)]
const TokenElevation: i32 = 20; // TOKEN_ELEVATION

/// 对应 Win32 `TOKEN_ELEVATION`。它的唯一成员是 **TokenIsElevated**
/// （非零 = 已提升），不是 TokenIsFiltered——两者语义正好相反，
/// 早先把字段写成 token_is_filtered 容易让人以为判断逻辑反了而去"修"它。
#[repr(C)]
struct TokenElevation {
    token_is_elevated: DWORD,
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
            token_is_elevated: 0,
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
        ok == TRUE && elev.token_is_elevated != 0
    }
}
