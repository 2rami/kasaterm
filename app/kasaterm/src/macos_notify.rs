//! macOS 알림 **클릭** 처리 — 배너를 누르면 그 pane 으로 간다.
//!
//! `UNUserNotificationCenter` 는 delegate 로만 클릭을 알려준다. delegate 가 없으면
//! 배너를 눌러도 앱이 앞으로 나올 뿐 **어디로 갈지가 없다** — 학생을 여럿 돌리면
//! 알림이 하는 일이 「뭔가 끝났다」에서 끝나고, 어느 자리인지는 사이드바를 눈으로
//! 훑어 찾아야 했다(2026-08-11 조사에서 가장 크게 벌어진 격차).
//!
//! 배선의 절반은 이미 있었다 — `UserEvent::NotifyFocus` 를 받는 쪽은 `SocketFocus`
//! 와 같은 길(방 전환까지 처리)을 탄다. 여기서 하는 일은 클릭을 잡아 그 길에
//! 올려 주는 것뿐이다.
//!
//! ⚠️ **surface id 는 재사용된다.** 알림이 뜬 뒤 그 pane 이 닫히고 번호가 새 셸에
//! 넘어가면 엉뚱한 자리로 간다. 그래서 알림에 그때의 claude 세션 id 를 같이 실어
//! 두고, 받는 쪽에서 같은 세션일 때만 옮긴다 — 못 맞추면 **아무 데도 안 간다**.
//! 엉뚱한 pane 으로 끌려가는 것보다 아무 일도 안 일어나는 편이 낫다.

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{define_class, AllocAnyThread, DefinedClass};
use objc2_user_notifications::{
    UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

struct Ivars {
    proxy: EventLoopProxy<UserEvent>,
}

define_class!(
    // SAFETY:
    // - 상위 NSObject 는 서브클래싱 제약이 없다.
    // - NotifyDelegate 는 Drop 을 구현하지 않는다.
    #[unsafe(super(NSObject))]
    #[name = "KasatermNotifyDelegate"]
    #[ivars = Ivars]
    struct NotifyDelegate;

    unsafe impl NSObjectProtocol for NotifyDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotifyDelegate {
        /// 배너를 눌렀다(기본 동작·커스텀 액션 모두 여기로 온다).
        ///
        /// completion 을 **반드시** 불러야 시스템이 이 응답을 닫는다 — 안 부르면
        /// 다음 클릭이 씹힌다.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        unsafe fn did_receive_response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            let ident = response.notification().request().identifier().to_string();
            if let Some((pane, sid)) = route_from_identifier(&ident) {
                let _ = self
                    .ivars()
                    .proxy
                    .send_event(UserEvent::NotifyFocus { pane, sid });
            }
            completion.call(());
        }
    }
);

/// 알림 identifier 에서 갈 자리를 꺼낸다 — `kasaterm-notify-{seq}|{pane}|{sid}`.
///
/// `userInfo`(NSDictionary) 대신 identifier 를 쓰는 이유: 실어야 하는 게 짧은 문자열
/// 둘뿐인데, 딕셔너리를 세우려면 objc2 의 키 타입 제약(`NSCopying`)에 맞춘 캐스팅이
/// 줄줄이 붙는다. identifier 는 우리가 정하는 문자열이라 그 무게가 없다.
///
/// 자리 표시(`kasaterm-notify-3||`)나 형식이 안 맞는 것은 None — **못 읽으면 아무 데도
/// 안 간다**가 이 경로의 규칙이다.
fn route_from_identifier(ident: &str) -> Option<(String, Option<String>)> {
    let mut it = ident.split('|');
    let _prefix = it.next()?;
    let pane = it.next().filter(|s| !s.is_empty())?.to_string();
    let sid = it.next().filter(|s| !s.is_empty()).map(str::to_string);
    Some((pane, sid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_carries_pane_and_session() {
        assert_eq!(
            route_from_identifier("kasaterm-notify-7|%116|abc-123"),
            Some(("%116".into(), Some("abc-123".into())))
        );
        // 세션을 못 실은 알림(순정 셸 pane).
        assert_eq!(
            route_from_identifier("kasaterm-notify-7|%116|"),
            Some(("%116".into(), None))
        );
        // 갈 자리가 아예 없는 알림 — 눌러도 아무 데도 안 간다.
        assert_eq!(route_from_identifier("kasaterm-notify-7"), None);
        assert_eq!(route_from_identifier("kasaterm-notify-7||"), None);
    }
}

// ⚠️ 구식(NSUserNotification) 경로는 시도했고 **막혀 있다** — 다시 시도하지 말 것.
// 새 체계(UN)는 자체 서명 'kasaterm-dev' 번들의 등록 요청을 "Notifications are
// not allowed for this application" 으로 거절하고(2026-08-17 격리 인스턴스 +
// KASATERM_AUTONOTIFY_MS 실측), 구식 센터의 deliverNotification 은 같은 검문에
// **예외도 오류도 없이 조용히 버려진다**(알림 DB 그룹컨테이너 db2 에 기록이 안
// 남는 것으로 실측 — osascript 배달은 scripteditor2 명의로 남는다). 앱 아이콘을
// 알림에 실으려면 애플 발급 인증서 서명 또는 자체 배너 창뿐이다.

/// 알림 클릭 핸들러를 건다. 프로세스당 한 번.
///
/// **알림이 배달되기 전에** 걸려야 한다 — delegate 가 없는 동안 눌린 알림은 그냥
/// 앱만 깨우고 사라진다. 그래서 권한 요청과 같은 자리(부팅 직후)에서 부른다.
///
/// delegate 는 시스템이 약참조로 들고 있으므로 **여기서 leak 해 살려 둔다**. 프로세스
/// 수명과 같아 회수할 일이 없고, 회수하면 그 순간부터 클릭이 조용히 죽는다.
pub(crate) fn install_notification_click_handler(proxy: EventLoopProxy<UserEvent>) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // ⚠️ **번들 밖에서는 부르면 안 된다.** `currentNotificationCenter` 는 프로세스가
        // 앱 번들 안에 있을 때만 살아서, `cargo run` 의 `target/debug/kasaterm` 에서는
        // `bundleProxyForCurrentProcess is nil` 로 **ObjC 예외를 던지고 프로세스를
        // 죽인다**. Rust panic 이 아니라 `catch_unwind` 로도 못 잡는다 — 부르고 나면
        // 이미 늦으니 **부르기 전에** 갈라야 한다.
        //
        // 이걸 빠뜨려 개발 실행이 부팅 즉시 죽었다(2026-08-11). 이 레포의 자율 테스트가
        // 전부 `cargo run` 위에 서 있어서, 그 사이 모든 pane 이 스크린샷 검증을 못 했다.
        if !crate::chrome::is_bundled() {
            return;
        }
        let this = NotifyDelegate::alloc().set_ivars(Ivars { proxy });
        let this: Retained<NotifyDelegate> = unsafe { objc2::msg_send![super(this), init] };
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let proto = objc2::runtime::ProtocolObject::from_ref(&*this);
        center.setDelegate(Some(proto));
        // 시스템이 약참조로 들고 있으니 우리가 살려 둔다.
        std::mem::forget(this);
    });
}
