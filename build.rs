fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/app.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=winresource failed: {e}");
        }
    }
}
