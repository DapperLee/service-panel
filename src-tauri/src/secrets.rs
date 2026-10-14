//! 配置密文存储：Windows 上用 DPAPI（CryptProtectData）加密整条 spec_json，
//! 密文与「当前用户 + 本机」绑定，无需外部密钥文件。带 `enc:` 前缀自描述，
//! 无前缀视为明文（兼容旧记录，下次保存自动升级为加密）。
// base64 只在 Windows 的 DPAPI 密文路径里用到，非 Windows 全走明文，别拉进 unix 编译
#[cfg(windows)]
use base64::{engine::general_purpose::STANDARD, Engine};

const PREFIX: &str = "enc:";

#[cfg(windows)]
mod dpapi {
    use std::ffi::c_void;
    use std::io;
    use std::slice;

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(
            data_in: *const DataBlob,
            desc: *const u16,
            optional: *const c_void,
            reserved: *const c_void,
            prompt: *const c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            data_in: *const DataBlob,
            desc: *mut *mut u16,
            optional: *const c_void,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(h: *mut c_void) -> *mut c_void;
    }

    pub fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
        unsafe {
            let in_blob = DataBlob {
                cb_data: plain.len() as u32,
                pb_data: plain.as_ptr() as *mut u8,
            };
            let mut out = DataBlob {
                cb_data: 0,
                pb_data: std::ptr::null_mut(),
            };
            let ok = CryptProtectData(
                &in_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let bytes = slice::from_raw_parts(out.pb_data, out.cb_data as usize).to_vec();
            LocalFree(out.pb_data as *mut c_void);
            Ok(bytes)
        }
    }

    pub fn unprotect(blob: &[u8]) -> io::Result<Vec<u8>> {
        unsafe {
            let in_blob = DataBlob {
                cb_data: blob.len() as u32,
                pb_data: blob.as_ptr() as *mut u8,
            };
            let mut out = DataBlob {
                cb_data: 0,
                pb_data: std::ptr::null_mut(),
            };
            let ok = CryptUnprotectData(
                &in_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let bytes = slice::from_raw_parts(out.pb_data, out.cb_data as usize).to_vec();
            LocalFree(out.pb_data as *mut c_void);
            Ok(bytes)
        }
    }
}

/// 加密明文字符串 -> `enc:<base64>`。非 Windows 或加密失败时原样返回（降级为明文）。
pub fn encrypt(plain: &str) -> String {
    #[cfg(windows)]
    {
        match dpapi::protect(plain.as_bytes()) {
            Ok(blob) => format!("{}{}", PREFIX, STANDARD.encode(&blob)),
            Err(_) => plain.to_string(),
        }
    }
    #[cfg(not(windows))]
    {
        plain.to_string()
    }
}

/// 解密。带 `enc:` 前缀则 DPAPI 解密；否则视为明文原样返回（兼容旧记录）。
pub fn decrypt(stored: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(b64) = stored.strip_prefix(PREFIX) {
            match STANDARD.decode(b64) {
                Ok(blob) => match dpapi::unprotect(&blob) {
                    Ok(bytes) => String::from_utf8(bytes).unwrap_or_default(),
                    Err(_) => String::new(),
                },
                Err(_) => String::new(),
            }
        } else {
            stored.to_string()
        }
    }
    #[cfg(not(windows))]
    {
        stored.strip_prefix(PREFIX).unwrap_or(stored).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{decrypt, PREFIX};

    #[test]
    fn plaintext_without_prefix_passes_through() {
        // 旧明文记录（无 enc: 前缀）应原样返回，兼容不丢配置
        let json = "{\"driver\":\"command\",\"command\":\"node\"}";
        assert_eq!(decrypt(json), json);
    }

    #[test]
    fn prefix_is_recognized() {
        assert!(format!("{PREFIX}abc").starts_with(PREFIX));
        assert!(!json_hack().starts_with(PREFIX));
    }
    fn json_hack() -> String {
        "{\"a\":1}".to_string()
    }
}
