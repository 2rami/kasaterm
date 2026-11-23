use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::autoreleasepool;
use objc2::runtime::Bool;
use objc2_core_graphics::{CGEventSource, CGEventSourceStateID, CGEventType};
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LABiometryType, LAContext, LAError, LAPolicy};

use super::{Origin, REF_PREFIX};

const PRESENT_IF_INPUT_WITHIN: f64 = 180.0;
const PROMPT_WAIT: Duration = Duration::from_secs(25);
const REQUESTER_CHECK: Duration = Duration::from_millis(250);
static PROMPT: Mutex<()> = Mutex::new(());

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Decision {
    Approved,
    Phone,
    Cancelled,
    Denied,
    Busy,
}

fn prompt_reason(origin: &Origin, refs: &[String]) -> Option<String> {
    let items = refs.iter()
        .map(|item| format!("{:?}", item.strip_prefix(REF_PREFIX).unwrap_or(item)))
        .collect::<Vec<_>>()
        .join(", ");
    let actor = if origin.student.is_empty() { &origin.pane } else { &origin.student };
    let reason = format!("{} 키체인 사용 허용\n{items}", super::clip(actor, 24));
    // The system alert gives us only a title. Longer requests need the phone's full list.
    (reason.chars().count() <= 120).then_some(reason)
}

fn reply_decision(authenticated: bool, error: Option<LAError>) -> Decision {
    if authenticated {
        return Decision::Approved;
    }
    match error {
        Some(LAError::UserFallback | LAError::SystemCancel | LAError::BiometryNotAvailable |
            LAError::BiometryNotEnrolled | LAError::BiometryLockout | LAError::NotInteractive |
            LAError::BiometryNotPaired | LAError::BiometryDisconnected) => Decision::Phone,
        Some(LAError::UserCancel | LAError::AppCancel) => Decision::Cancelled,
        _ => Decision::Denied,
    }
}

pub(super) fn approve(origin: &Origin, refs: &[String]) -> Decision {
    let Some(reason) = prompt_reason(origin, refs) else { return Decision::Phone };
    let idle = CGEventSource::seconds_since_last_event_type(CGEventSourceStateID::HIDSystemState, CGEventType(u32::MAX));
    if !(0.0..PRESENT_IF_INPUT_WITHIN).contains(&idle) {
        return Decision::Phone;
    }
    if !(origin.alive)() {
        return Decision::Cancelled;
    }
    autoreleasepool(|_| {
        let policy = LAPolicy::DeviceOwnerAuthenticationWithBiometrics;
        let context = unsafe { LAContext::new() };
        if unsafe { context.canEvaluatePolicy_error(policy).is_err() || context.biometryType() != LABiometryType::TouchID } {
            return Decision::Phone;
        }
        let Ok(_prompt) = PROMPT.try_lock() else { return Decision::Busy };
        let (send, receive) = mpsc::channel();
        // The NSError belongs to this callback. Only its code crosses threads.
        let reply = RcBlock::new(move |success: Bool, error: *mut NSError| {
            let error = unsafe { error.as_ref() }.map(|error| LAError(error.code()));
            let _ = send.send(reply_decision(success.as_bool(), error));
        });
        let deadline = Instant::now() + PROMPT_WAIT;
        unsafe {
            // A recent unlock must not approve a different credential request.
            context.setTouchIDAuthenticationAllowableReuseDuration(0.0);
            context.setLocalizedFallbackTitle(Some(&NSString::from_str("폰으로 승인")));
            context.evaluatePolicy_localizedReason_reply(policy, &NSString::from_str(&reason), &reply);
        }
        let decision = loop {
            if !(origin.alive)() {
                break Decision::Cancelled;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break Decision::Phone;
            }
            match receive.recv_timeout(remaining.min(REQUESTER_CHECK)) {
                Ok(decision) if Instant::now() < deadline => break decision,
                Ok(_) => break Decision::Phone,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break Decision::Denied,
            }
        };
        // Close the local prompt before using a different approval channel.
        unsafe { context.invalidate() };
        decision
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_system_success_approves_and_user_cancel_does_not_send_to_phone() {
        assert_eq!(reply_decision(true, None), Decision::Approved);
        assert_eq!(reply_decision(false, None), Decision::Denied);
        assert_eq!(reply_decision(false, Some(LAError::AuthenticationFailed)), Decision::Denied);
        assert_eq!(reply_decision(false, Some(LAError::UserCancel)), Decision::Cancelled);
        assert_eq!(reply_decision(false, Some(LAError::UserFallback)), Decision::Phone);
        assert_eq!(reply_decision(false, Some(LAError::BiometryLockout)), Decision::Phone);
    }

    #[test]
    fn local_prompt_shows_every_item_or_uses_the_phone_list() {
        let origin = Origin { pane: "%3".into(), student: String::new(), alive: Box::new(|| true) };
        let refs = ["keychain://web:example.com#demo".into(), "keychain://fixture-token".into()];
        let reason = prompt_reason(&origin, &refs).unwrap();
        assert!(reason.contains("%3"));
        assert!(reason.contains("web:example.com#demo"));
        assert!(reason.contains("fixture-token"));
        assert!(prompt_reason(&origin, &[format!("keychain://{}", "x".repeat(256))]).is_none());
    }
}
