fn main() {
    // Windows: 작업 표시줄·시작 메뉴가 기본 아이콘 대신 이 앱 아이콘을 쓰게 실행 파일에 박는다.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=../../assets/desk.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/desk.ico");
        res.set("FileDescription", "KasaDesk");
        if let Err(e) = res.compile() {
            println!("cargo:warning=winresource icon embed failed: {e}");
        }
    }
}
