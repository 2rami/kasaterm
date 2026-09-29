# 카사넷 C ABI 정적 라이브러리(tool/kasanet.sh 가 굽는다). Dart 는 DynamicLibrary.process() 로 기호를 찾으므로
# ① 아무도 부르지 않는 기호도 링크에 끌려 들어오게 -u 로 묶고 ② 앱 기호표에서 전역 기호를 지우지 않게 한다.
Pod::Spec.new do |s|
  s.name = 'KasaNet'
  s.version = '0.1.0'
  s.summary = 'kasanet iroh P2P entrance for the phone app'
  s.homepage = 'https://github.com/2rami/kasaterm'
  s.license = { :type => 'MIT' }
  s.author = 'kasaterm'
  s.source = { :path => '.' }
  s.platform = :ios, '15.0'
  s.vendored_frameworks = 'KasaNet.xcframework'
  s.frameworks = 'SystemConfiguration', 'Security'
  s.libraries = 'c++'
  syms = %w[kasanet_start kasanet_id kasanet_open kasanet_state kasanet_close
            kasanet_network_changed kasanet_stop kasanet_last_error kasanet_free_string]
  s.user_target_xcconfig = {
    'OTHER_LDFLAGS' => syms.map { |f| "-Wl,-u,_#{f}" }.join(' '),
    'STRIP_STYLE' => 'non-global',
  }
end
