fn main() {
    // Windows: 파일 판(FileVersion)을 서비스 판으로 박는다 — MSI 는 판이 같은 실행 파일을 갈아 끼우지 않아 앱만 바뀐
    // 업데이트에서 서비스가 멈추지 않는다(`docs/screen-service-split.md` 「Windows — MSI」).
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set("FileDescription", "kasaterm screen service");
        if let Err(e) = res.compile() {
            println!("cargo:warning=winresource version embed failed: {e}");
        }
    }
}
