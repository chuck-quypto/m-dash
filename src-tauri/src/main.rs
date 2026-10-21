#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--claude-statusline")) {
        m_dash_lib::claude_feed::run();
        return;
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("GDK_BACKEND").is_none() && std::env::var_os("DISPLAY").is_some() {
        // Native Wayland cannot provide the strip's minimize/position/stacking contract.
        // Set this before GTK or worker threads start; explicit backend choices win.
        std::env::set_var("GDK_BACKEND", "x11");
    }
    m_dash_lib::run();
}
