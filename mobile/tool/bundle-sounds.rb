#!/usr/bin/env ruby
# Runner/Sounds/*.caf 를 Runner 타깃의 Copy Bundle Resources 에 넣는다(여러 번 돌려도 같다).
# 푸시 알림음은 앱 본체 번들 루트에 있어야 UNNotificationSound(named:) 가 찾는다.
#   gem install --user-install xcodeproj && ruby tool/bundle-sounds.rb
require 'xcodeproj'
root = File.expand_path('..', __dir__)
project = Xcodeproj::Project.open(File.join(root, 'ios/Runner.xcodeproj'))
target = project.targets.find { |t| t.name == 'Runner' } or abort 'Runner 타깃 없음'
runner_group = project.main_group['Runner'] or abort 'Runner 그룹 없음'
sounds_group = runner_group['Sounds'] || runner_group.new_group('Sounds', 'Sounds')
added = 0
Dir[File.join(root, 'ios/Runner/Sounds/*.caf')].sort.each do |path|
  name = File.basename(path)
  ref = sounds_group.files.find { |f| f.path == name } || sounds_group.new_file(name)
  next if target.resources_build_phase.files_references.include?(ref)
  target.resources_build_phase.add_file_reference(ref)
  added += 1
end
project.save
puts "resources 에 추가 #{added}개 (전체 #{sounds_group.files.size}개)"
