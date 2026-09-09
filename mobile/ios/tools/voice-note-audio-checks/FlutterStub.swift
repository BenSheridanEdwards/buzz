import Foundation

// Stands in for the Flutter module so `VoiceNotePackager.swift` compiles
// verbatim, with `import Flutter` unchanged, outside an Xcode build.
// Only the two symbols the packager uses are declared; nothing here is part
// of the app.

public typealias FlutterResult = (Any?) -> Void

public class FlutterError: NSObject {
  public let code: String
  public let message: String?
  public let details: Any?

  public init(code: String, message: String?, details: Any?) {
    self.code = code
    self.message = message
    self.details = details
  }
}
