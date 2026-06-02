fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        #[cfg(target_os = "windows")]
        {
            let mut res = winres::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            if let Err(e) = res.compile() {
                eprintln!("Failed to compile Windows resources: {}", e);
                std::process::exit(1);
            }
        }
    }
}
