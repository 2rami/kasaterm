// personacol은 Board 통합 커밋과 충돌하지 않게 아직 main.rs에 연결하지 않는다.
// 이 독립 테스트 타깃이 그 사이에도 모듈 전체와 순수 상태 검증을 컴파일한다.
#![allow(dead_code)]

#[path = "../src/personacol.rs"]
mod personacol;
