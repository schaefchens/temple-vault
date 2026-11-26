Pod::Spec.new do |s|
  s.name           = 'NativeReads'
  s.version        = '1.0.0'
  s.summary        = "The demo's native code reading a space through TempleReader"
  s.license        = 'Apache-2.0'
  s.author         = 'schaefchens'
  s.homepage       = 'https://github.com/schaefchens/temple-vault'
  s.platforms      = { :ios => '16.4' }
  s.swift_version  = '5.9'
  s.source         = { git: 'https://github.com/schaefchens/temple-vault.git' }
  s.static_framework = true

  s.dependency 'ExpoModulesCore'
  s.dependency 'TempleReactNative'

  s.source_files = '**/*.swift'
end
