import Foundation

/// Reads the objects of a space that JavaScript opened – for the app's own native code, such as a
/// media player's resource loader – while the space is open. The handle comes from
/// `space.nativeReader()` in JavaScript. Each call waits its turn among the space's calls and
/// blocks until done: call from any thread, best not the main one.
public struct TempleReader: Sendable {
  public let handle: String

  public init(handle: String) {
    self.handle = handle
  }

  /// An object's size in bytes.
  public func objectLength(_ id: Int64) throws -> UInt64 {
    var length: UInt64 = 0
    try call { message, capacity in __temple_reader_object_length(handle, id, &length, message, capacity) }
    return length
  }

  /// Bytes `start..<end` of an object, up to its end if that comes first – none from there on.
  /// Wiped once the `Data` is released.
  public func readObject(_ id: Int64, start: UInt64, end: UInt64) throws -> Data {
    var bytes: UnsafeMutablePointer<UInt8>?
    var count = 0
    try call { message, capacity in
      __temple_reader_read_object(handle, id, start, end, &bytes, &count, message, capacity)
    }
    guard let bytes, count > 0 else {
      __temple_reader_free(bytes, count)
      return Data()
    }
    return Data(bytesNoCopy: bytes, count: count, deallocator: .custom { pointer, count in
      __temple_reader_free(pointer.assumingMemoryBound(to: UInt8.self), count)
    })
  }

  private func call(_ body: (UnsafeMutablePointer<CChar>, Int) -> Int32) throws {
    var message = [CChar](repeating: 0, count: 256)
    let kind = message.withUnsafeMutableBufferPointer { body($0.baseAddress!, $0.count) }
    if kind != 0 {
      throw TempleReaderError(
        kind: TempleReaderError.Kind(code: kind),
        message: message.withUnsafeBufferPointer { String(cString: $0.baseAddress!) })
    }
  }
}

/// Why a `TempleReader` call failed.
public struct TempleReaderError: Error, CustomStringConvertible {
  /// As TempleError's kinds in JavaScript.
  public enum Kind: String, Sendable {
    /// The space has locked, or the handle was given up.
    case locked = "Locked"
    case noSuchObject = "NoSuchObject"
    case damaged = "Damaged"
    case database = "Database"
    case io = "Io"

    /// As bindings/ffi/src/native.rs numbers them.
    init(code: Int32) {
      let kinds: [Kind] = [.locked, .noSuchObject, .damaged, .database, .io]
      self = code >= 1 && Int(code) <= kinds.count ? kinds[Int(code) - 1] : .io
    }
  }

  public let kind: Kind
  public let message: String

  public var description: String { "\(kind.rawValue): \(message)" }
}
