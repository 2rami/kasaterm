//! 트레이의 카사펫 칸 — 바탕화면 펫(kasapet)을 켜고 끄고, 보이고 숨기고, 크기·말소리를 고른다. 펫은 Dock 과 ⌘Tab 에
//! 뜨지 않는 앱이라 이 칸이 그 앱의 메뉴다(2026-11-27 「카사펫 카사텀 트레이에 합쳐줘, 거기서 설정 같은 거 할 수 있게」).
//! 켜고 끄고 숨기기는 macOS 가 남의 앱에 해 주는 대로(NSRunningApplication) 하고, 크기·말소리는 펫이 듣는 분산 알림
//! `com.kasa.kasapet.control` 로 보낸다. 체크는 펫이 제 설정(com.kasa.kasapet 의 zoom·volume)에 남긴 값을 읽어 맞춘다.

use std::time::{Duration, Instant};

use muda::{CheckMenuItem, IsMenuItem, MenuId, MenuItem, Submenu};
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::{NSDictionary, NSDistributedNotificationCenter, NSNumber, NSString, NSUserDefaults};

const BUNDLE: &str = "com.kasa.kasapet";
const CONTROL: &str = "com.kasa.kasapet.control";

/// 크기는 펫의 기본 배율(설정 zoom 2.4)이 「보통」, 말소리는 원래 소리가 「보통」이다.
const SIZES: [(&str, f64); 3] = [("작게", 1.6), ("보통", 2.4), ("크게", 3.4)];
const VOICES: [(&str, f64); 3] = [("끄기", 0.0), ("작게", 0.4), ("보통", 1.0)];

/// 펫을 다시 읽는 사이 — 메뉴가 열릴 때를 알 길이 없어 이만큼마다 맞춰 둔다.
const READ_EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PetState {
    running: bool,
    hidden: bool,
    zoom: Option<f64>,
    volume: Option<f64>,
}

/// 메뉴가 보일 모습. 펫이 꺼져 있으면 켜기만 누를 수 있다.
#[derive(Debug, PartialEq)]
struct View {
    toggle: &'static str,
    live: bool,
    shown: bool,
    size: usize,
    voice: usize,
}

fn view(state: PetState) -> View {
    View {
        toggle: if state.running { "카사펫 끄기" } else { "카사펫 켜기" },
        live: state.running,
        shown: state.running && !state.hidden,
        // 휠로 키운 크기는 가장 가까운 것에 체크한다.
        size: nearest(&SIZES, state.zoom.unwrap_or(SIZES[1].1)),
        voice: nearest(&VOICES, state.volume.unwrap_or(VOICES[2].1)),
    }
}

fn nearest(choices: &[(&str, f64)], value: f64) -> usize {
    (0..choices.len())
        .min_by(|&a, &b| (choices[a].1 - value).abs().total_cmp(&(choices[b].1 - value).abs()))
        .unwrap_or(0)
}

pub(crate) struct PetMenu {
    toggle: MenuItem,
    shown: CheckMenuItem,
    size: Submenu,
    sizes: Vec<CheckMenuItem>,
    voice: Submenu,
    voices: Vec<CheckMenuItem>,
    next_read: Instant,
}

impl PetMenu {
    pub(crate) fn new() -> Option<Self> {
        let checks = |choices: &[(&str, f64)]| -> Vec<CheckMenuItem> {
            choices.iter().map(|(name, _)| CheckMenuItem::new(*name, true, false, None)).collect()
        };
        let sizes = checks(&SIZES);
        let voices = checks(&VOICES);
        let size = Submenu::new("카사펫 크기", true);
        let voice = Submenu::new("카사펫 말소리", true);
        for item in &sizes {
            size.append(item).ok()?;
        }
        for item in &voices {
            voice.append(item).ok()?;
        }
        let mut menu = PetMenu {
            toggle: MenuItem::new("카사펫 켜기", true, None),
            shown: CheckMenuItem::new("카사펫 보이기", true, true, None),
            size,
            sizes,
            voice,
            voices,
            next_read: Instant::now(),
        };
        menu.refresh();
        Some(menu)
    }

    pub(crate) fn items(&self) -> [&dyn IsMenuItem; 4] {
        [&self.toggle, &self.shown, &self.size, &self.voice]
    }

    /// 때가 되면 펫을 다시 읽어 메뉴를 맞춘다.
    pub(crate) fn refresh(&mut self) {
        if Instant::now() < self.next_read {
            return;
        }
        self.next_read = Instant::now() + READ_EVERY;
        let v = view(read());
        self.toggle.set_text(v.toggle);
        self.shown.set_enabled(v.live);
        self.shown.set_checked(v.shown);
        self.size.set_enabled(v.live);
        self.voice.set_enabled(v.live);
        for (i, item) in self.sizes.iter().enumerate() {
            item.set_checked(i == v.size);
        }
        for (i, item) in self.voices.iter().enumerate() {
            item.set_checked(i == v.voice);
        }
    }

    /// 이 칸의 항목이면 그 일을 하고 참.
    pub(crate) fn handle(&mut self, id: &MenuId) -> bool {
        let pet = running();
        if self.toggle.id() == id {
            match pet {
                Some(app) => {
                    app.terminate();
                }
                None => launch(),
            }
        } else if self.shown.id() == id {
            if let Some(app) = pet {
                if app.isHidden() {
                    app.unhide();
                } else {
                    app.hide();
                }
            }
        } else if let Some(i) = self.sizes.iter().position(|item| item.id() == id) {
            control("zoom", SIZES[i].1);
        } else if let Some(i) = self.voices.iter().position(|item| item.id() == id) {
            control("volume", VOICES[i].1);
        } else {
            return false;
        }
        // muda 는 누른 체크를 제 손으로 뒤집는다 — 바로 다시 읽어 펫의 실제 값으로 맞춘다.
        self.next_read = Instant::now();
        self.refresh();
        self.next_read = Instant::now() + Duration::from_millis(400);
        true
    }
}

fn running() -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(BUNDLE))
        .iter()
        .find(|app| !app.isTerminated())
}

fn read() -> PetState {
    let app = running();
    let defaults = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&NSString::from_str(BUNDLE)));
    let number = |key: &str| {
        let value = defaults.as_ref()?.objectForKey(&NSString::from_str(key))?;
        value.downcast::<NSNumber>().ok().map(|n| n.as_f64())
    };
    PetState {
        running: app.is_some(),
        hidden: app.as_ref().is_some_and(|a| a.isHidden()),
        zoom: number("zoom"),
        volume: number("volume"),
    }
}

/// 펫을 띄운다 — 앞으로 가져오지 않고(`-g`), 앱이 어디 있든 LaunchServices 가 아는 그 번들로.
fn launch() {
    match crate::proc::command("/usr/bin/open").args(["-g", "-b", BUNDLE]).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("[tray] 카사펫을 못 띄웠다: {e}"),
    }
}

fn control(key: &str, value: f64) {
    let info = NSDictionary::from_slices(&[&*NSString::from_str(key)], &[&*NSNumber::new_f64(value)]);
    // SAFETY: 키는 NSString, 값은 NSNumber 인 사전이라 userInfo 가 받는 plist 사전 그대로다.
    let info: Retained<NSDictionary> = unsafe { Retained::cast_unchecked(info) };
    let center = NSDistributedNotificationCenter::defaultCenter();
    // SAFETY: 이름은 NSString, object 는 없음, userInfo 는 위의 plist 사전이다.
    unsafe {
        center.postNotificationName_object_userInfo_deliverImmediately(&NSString::from_str(CONTROL), None, Some(&info), true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pet_that_is_off_can_only_be_started() {
        let v = view(PetState::default());
        assert_eq!(v.toggle, "카사펫 켜기");
        assert!(!v.live && !v.shown);
        let v = view(PetState { running: true, hidden: true, ..PetState::default() });
        assert_eq!(v.toggle, "카사펫 끄기");
        assert!(v.live && !v.shown);
        assert!(view(PetState { running: true, ..PetState::default() }).shown);
    }

    #[test]
    fn checks_follow_what_the_pet_saved() {
        assert_eq!(view(PetState::default()).size, 1, "no saved zoom is the pet's own size");
        assert_eq!(view(PetState::default()).voice, 2, "no saved volume is the full voice");
        let v = view(PetState { running: true, zoom: Some(3.9), volume: Some(0.0), ..PetState::default() });
        assert_eq!((v.size, v.voice), (2, 0));
        assert_eq!(view(PetState { zoom: Some(1.9), ..PetState::default() }).size, 0, "a wheel zoom checks the nearest");
    }
}
