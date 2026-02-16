#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use self::linux::*;
#[cfg(target_os = "macos")]
pub use self::macos::*;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("unsupported target OS: ClawShell currently supports only Linux and macOS");
