package de.schaefchens.temple;

import java.io.IOException;

/** Why a {@link TempleReader} call failed – an IOException, as a media player's data source throws. */
public final class TempleReaderException extends IOException {
  /** As TempleError's kinds in JavaScript. */
  public enum Kind {
    /** The space has locked, or the handle was given up. */
    LOCKED,
    NO_SUCH_OBJECT,
    DAMAGED,
    DATABASE,
    IO
  }

  public final Kind kind;

  private TempleReaderException(Kind kind, String message) {
    super(message);
    this.kind = kind;
  }

  /** From the native side's "kind message", the kind numbered as in bindings/ffi/src/native.rs. */
  static TempleReaderException from(IOException e) {
    String text = e.getMessage() == null ? "" : e.getMessage();
    int space = text.indexOf(' ');
    Kind[] kinds = Kind.values();
    int code;
    try {
      code = Integer.parseInt(space < 0 ? text : text.substring(0, space));
    } catch (NumberFormatException notOurs) {
      code = 0;
    }
    if (code < 1 || code > kinds.length) {
      TempleReaderException failed = new TempleReaderException(Kind.IO, text);
      failed.initCause(e);
      return failed;
    }
    return new TempleReaderException(kinds[code - 1], text.substring(space + 1));
  }
}
