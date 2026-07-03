fn main() {
    wdk_build::Config::from_env_auto()
        .expect("WDK 빌드 설정 실패")
        .configure_binary_build()
        .expect("바이너리 빌드 설정 실패");
}
