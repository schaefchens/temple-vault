# Temple's own since it compiles TempleReader: ubrn.config.yaml keeps uniffi-bindgen-react-native
# from writing it again.
require "json"

package = JSON.parse(File.read(File.join(__dir__, "package.json")))

Pod::Spec.new do |s|
  s.name         = "TempleReactNative"
  s.version      = package["version"]
  s.summary      = package["description"]
  s.homepage     = package["homepage"]
  s.license      = package["license"]
  s.authors      = package["author"]

  s.platforms    = { :ios => min_ios_version_supported }
  s.source       = { :git => "https://github.com/schaefchens/temple-vault.git", :tag => "#{s.version}" }

  # The Rust library as a dynamic framework: CocoaPods embeds it in the app
  # and signs it with the app's identity.
  s.vendored_frameworks = "ios/temple_ffi.xcframework"
  # TempleReader, for the app's own native code: Swift over what the framework exports.
  s.source_files = "ios/*.{h,swift}"
  s.public_header_files = "ios/*.h"
  s.swift_version = "5.9"
  # No dependency on UbjsReactNative, the player that compiles the code JavaScript calls: a
  # Swift pod can't depend on it, as it defines no module. src/index.tsx checks for it instead.
end
