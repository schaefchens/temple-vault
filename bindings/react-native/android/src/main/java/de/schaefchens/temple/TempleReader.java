package de.schaefchens.temple;

import java.io.IOException;

/**
 * Reads the objects of a space that JavaScript opened – for the app's own native code, such as a
 * media player's data source – while the space is open. The handle comes from
 * {@code space.nativeReader()} in JavaScript. Each call waits its turn among the space's calls and
 * blocks until done: call from any thread, best not the main one.
 */
public final class TempleReader {
  static {
    // The library @ubjs/react-native opened for JavaScript: the same one, holding the same spaces.
    System.loadLibrary("temple_ffi");
  }

  private final String handle;

  public TempleReader(String handle) {
    if (handle == null) throw new NullPointerException("handle");
    this.handle = handle;
  }

  public String getHandle() {
    return handle;
  }

  /** An object's size in bytes. */
  public long objectLength(long id) throws TempleReaderException {
    try {
      return nativeObjectLength(handle, id);
    } catch (IOException e) {
      throw TempleReaderException.from(e);
    }
  }

  /** Bytes {@code start} to {@code end} of an object, up to its end if that comes first – none from there on. */
  public byte[] readObject(long id, long start, long end) throws TempleReaderException {
    if (start < 0 || end < 0) throw new IllegalArgumentException("start and end can't be negative");
    try {
      return nativeReadObject(handle, id, start, end);
    } catch (IOException e) {
      throw TempleReaderException.from(e);
    }
  }

  /**
   * Up to {@code length} bytes of an object from {@code position} into {@code buffer} at {@code offset}, as a media
   * player's data source reads; returns how many – fewer only at the object's end, 0 from there on.
   */
  public int readObject(long id, long position, byte[] buffer, int offset, int length) throws TempleReaderException {
    if (position < 0 || offset < 0 || length < 0 || offset > buffer.length - length) {
      throw new IndexOutOfBoundsException("position " + position + ", offset " + offset + ", length " + length);
    }
    if (length == 0) return 0;
    try {
      return nativeReadInto(handle, id, position, buffer, offset, length);
    } catch (IOException e) {
      throw TempleReaderException.from(e);
    }
  }

  // bindings/ffi/src/native.rs: they fail with an IOException, "kind message".
  private static native long nativeObjectLength(String handle, long id) throws IOException;

  private static native byte[] nativeReadObject(String handle, long id, long start, long end) throws IOException;

  private static native int nativeReadInto(
      String handle, long id, long position, byte[] buffer, int offset, int length) throws IOException;
}
