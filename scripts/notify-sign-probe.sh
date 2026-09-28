#!/usr/bin/env bash
# 알림센터 서명 실험 — 개인 팀 Developer ID 로 서명한 최소 앱이 macOS 알림 권한을 받는가.
#
# 2026-08-21 조사에서 자체 서명 번들은 `requestAuthorization` 이 「Notifications are not allowed for this
# application」으로 거절됐고, 남은 변수는 애플 발급 인증서(TeamIdentifier) 하나였다. 이 스크립트가 그
# 변수만 바꿔 A/B 로 잰다: 같은 코드를 자체 서명(kasaterm-dev)과 개인 팀 인증서로 각각 서명해 띄운다.
#
#   scripts/notify-sign-probe.sh            두 번들을 굽고 띄워 결과를 찍는다
#   KASATERM_SIGN_TEAM=XXXX ...             다른 팀(기본 L366799VND). 회사 팀으로는 서명하지 않는다
#
# 결과 줄: AUTH granted=<0|1> error=<…>  /  STATUS <0 미결정|1 거부|2 허용|3 임시> at <초>
# 권한을 받을 수 있는 번들이면 macOS 가 「알림 허용」을 묻고 AUTH 줄은 답할 때까지 안 온다(NOTIFY_PROBE_SECONDS, 기본 20초).
set -euo pipefail
TEAM="${KASATERM_SIGN_TEAM:-L366799VND}"
OUT="${1:-$(mktemp -d -t kasaterm-notify-probe)}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
# 번들 id 를 실행마다 새로 — 한 번 거절·허용된 id 는 알림 설정에 남아 다음 판정을 흐린다.
RUN="$(date +%s)"

team_of() {
  security find-certificate -c "$1" -p "$KEYCHAIN" 2>/dev/null \
    | openssl x509 -noout -subject 2>/dev/null | sed -nE 's/.*OU *= *([A-Z0-9]+).*/\1/p'
}
# 인증서 이름 괄호 안 값은 팀이 아닐 수 있어 OU 로 고른다(회사 맥북 키체인엔 회사 팀 인증서도 있다).
APPLE_ID=""
while IFS= read -r line; do
  name=$(echo "$line" | sed -E 's/^[^"]*"([^"]*)".*$/\1/')
  if [[ "$(team_of "$name")" == "$TEAM" ]]; then APPLE_ID="$name"; break; fi
done < <(security find-identity -v -p codesigning "$KEYCHAIN" | grep '"Developer ID Application: ' || true)
[[ -n "$APPLE_ID" ]] || { echo "팀 $TEAM 의 Developer ID Application 인증서가 이 맥의 로그인 키체인에 없다" >&2; exit 2; }
python3 "$(dirname "$0")/signing-keychain.py" --probe "$KEYCHAIN" \
  || { echo "로그인 키체인의 서명 키가 codesign 에 안 열려 있다 — 여기서 서명하면 암호창에서 멈춘다" >&2; exit 2; }

cat > "$OUT/probe.m" <<'OBJC'
#import <Cocoa/Cocoa.h>
#import <UserNotifications/UserNotifications.h>
@interface D : NSObject <NSApplicationDelegate, UNUserNotificationCenterDelegate> @end
@implementation D
- (void)applicationDidFinishLaunching:(NSNotification *)n {
  UNUserNotificationCenter *c = [UNUserNotificationCenter currentNotificationCenter];
  c.delegate = self;
  [c requestAuthorizationWithOptions:(UNAuthorizationOptionAlert | UNAuthorizationOptionSound)
                   completionHandler:^(BOOL granted, NSError *e) {
    printf("AUTH granted=%d error=%s\n", granted, e ? e.localizedDescription.UTF8String : "none");
    fflush(stdout);
    if (!granted) return;
    UNMutableNotificationContent *body = [UNMutableNotificationContent new];
    body.title = @"카사텀 알림 실험";
    body.body = @"개인 Developer ID 로 서명한 번들이 보낸 알림이에요.";
    [c addNotificationRequest:[UNNotificationRequest requestWithIdentifier:@"probe" content:body trigger:nil]
        withCompletionHandler:^(NSError *e) {
      printf("POSTED error=%s\n", e ? e.localizedDescription.UTF8String : "none");
      fflush(stdout);
    }];
  }];
  const char *wait = getenv("NOTIFY_PROBE_SECONDS");
  int64_t seconds = wait ? atoll(wait) : 20;
  for (int64_t at = 3; at < seconds; at += 5) {
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, at * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
      [c getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings *s) {
        printf("STATUS %ld at %llds\n", (long)s.authorizationStatus, at);
        fflush(stdout);
      }];
    });
  }
  dispatch_after(dispatch_time(DISPATCH_TIME_NOW, seconds * NSEC_PER_SEC), dispatch_get_main_queue(), ^{ exit(0); });
}
- (void)userNotificationCenter:(UNUserNotificationCenter *)c willPresentNotification:(UNNotification *)n
         withCompletionHandler:(void (^)(UNNotificationPresentationOptions))done {
  done(UNNotificationPresentationOptionBanner | UNNotificationPresentationOptionList);
}
@end
int main(void) {
  NSApplication *app = [NSApplication sharedApplication];
  app.activationPolicy = NSApplicationActivationPolicyAccessory;
  D *d = [D new];
  app.delegate = d;
  [app run];
}
OBJC
clang -fobjc-arc -framework Cocoa -framework UserNotifications "$OUT/probe.m" -o "$OUT/probe"

# 번들은 ~/Applications 에 둔다 — 임시 폴더에서 띄우면 서명과 무관하게 같은 거절이 난다(2026-09-28 실측:
# 개인 Developer ID 번들도 /tmp 에서는 「not allowed」, ~/Applications 에서는 권한을 묻는다).
bundle() {  # <이름> <번들 id> <서명 신원>
  local app="$HOME/Applications/KasatermNotifyProbe-$1.app"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS"
  cp "$OUT/probe" "$app/Contents/MacOS/probe"
  cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>$2</string>
  <key>CFBundleExecutable</key><string>probe</string>
  <key>CFBundleName</key><string>카사텀 알림 실험</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
  codesign --force --options runtime --keychain "$KEYCHAIN" --sign "$3" "$app"
  echo "== $1 · $(codesign -dv "$app" 2>&1 | grep -E '^(Authority|TeamIdentifier)=' | head -2 | tr '\n' ' ')"
  open -n -W --stdout "$OUT/$1.out" --stderr "$OUT/$1.err" "$app"
  cat "$OUT/$1.out"
  # LaunchServices 에 이름이 쌓이지 않게 등록을 뺀다(같은 조사를 여러 번 하면 목록이 불어난다).
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u "$app" 2>/dev/null || true
  rm -rf "$app"
}

if security find-identity -p codesigning "$KEYCHAIN" | grep -q '"kasaterm-dev"'; then
  bundle self-signed "com.kasa.notify-probe.self.$RUN" kasaterm-dev
fi
bundle developer-id "com.kasa.notify-probe.$RUN" "$APPLE_ID"
echo "결과 폴더: $OUT"
