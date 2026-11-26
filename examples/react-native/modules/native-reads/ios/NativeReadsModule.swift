import ExpoModulesCore
import TempleReactNative

/// The demo's own native code reading a space JavaScript opened, as a media player would – for
/// Temple's self-test. Errors reach JavaScript with TempleError's kind as their code.
public class NativeReadsModule: Module {
  public func definition() -> ModuleDefinition {
    Name("NativeReads")

    AsyncFunction("objectLength") { (handle: String, id: Int) throws -> Double in
      try reading { Double(try TempleReader(handle: handle).objectLength(Int64(id))) }
    }

    /// In four parts, read on four threads at once.
    AsyncFunction("readObject") { (handle: String, id: Int, start: Double, end: Double) throws -> Data in
      try reading { try readInParts(TempleReader(handle: handle), Int64(id), UInt64(start), UInt64(end)) }
    }
  }
}

private func reading<T>(_ read: () throws -> T) throws -> T {
  do {
    return try read()
  } catch let error as TempleReaderError {
    throw Exception(name: "TempleReaderError", description: error.message, code: error.kind.rawValue)
  }
}

private final class Parts: @unchecked Sendable {
  let lock = NSLock()
  var read: [Result<Data, Error>]

  init(_ count: Int) {
    read = Array(repeating: .success(Data()), count: count)
  }
}

private func readInParts(_ reader: TempleReader, _ id: Int64, _ start: UInt64, _ end: UInt64) throws -> Data {
  let count = 4
  let size = end > start ? (end - start + UInt64(count) - 1) / UInt64(count) : 0
  let parts = Parts(count)
  DispatchQueue.concurrentPerform(iterations: count) { i in
    let from = start + UInt64(i) * size
    let read = Result { try reader.readObject(id, start: from, end: min(from + size, end)) }
    parts.lock.lock()
    parts.read[i] = read
    parts.lock.unlock()
  }
  var data = Data()
  for part in parts.read {
    data.append(try part.get())
  }
  return data
}
