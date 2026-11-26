package de.schaefchens.temple.demo.nativereads

import de.schaefchens.temple.TempleReader
import de.schaefchens.temple.TempleReaderException
import expo.modules.kotlin.exception.CodedException
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors

/**
 * The demo's own native code reading a space JavaScript opened, as a media player would – for Temple's self-test. Errors
 * reach JavaScript with TempleError's kind as their code.
 */
class NativeReadsModule : Module() {
  override fun definition() = ModuleDefinition {
    Name("NativeReads")

    AsyncFunction("objectLength") { handle: String, id: Long ->
      reading { TempleReader(handle).objectLength(id).toDouble() }
    }

    // In four parts, read on four threads at once.
    AsyncFunction("readObject") { handle: String, id: Long, start: Double, end: Double ->
      reading { readInParts(TempleReader(handle), id, start.toLong(), end.toLong()) }
    }
  }
}

private fun <T> reading(read: () -> T): T =
  try {
    read()
  } catch (e: TempleReaderException) {
    val kind = when (e.kind) {
      TempleReaderException.Kind.LOCKED -> "Locked"
      TempleReaderException.Kind.NO_SUCH_OBJECT -> "NoSuchObject"
      TempleReaderException.Kind.DAMAGED -> "Damaged"
      TempleReaderException.Kind.DATABASE -> "Database"
      else -> "Io"
    }
    throw CodedException(kind, e.message, e)
  }

private fun readInParts(reader: TempleReader, id: Long, start: Long, end: Long): ByteArray {
  val count = 4
  val size = if (end > start) (end - start + count - 1) / count else 0
  val threads = Executors.newFixedThreadPool(count)
  try {
    val parts = (0 until count).map { i ->
      threads.submit<ByteArray> {
        val from = start + i * size
        val to = minOf(from + size, end)
        if (i % 2 == 0) {
          reader.readObject(id, from, to)
        } else {
          // As a media player's data source reads: into its buffer, until the object ends.
          val buffer = ByteArray(maxOf(0L, to - from).toInt())
          var filled = 0
          while (filled < buffer.size) {
            val n = reader.readObject(id, from + filled, buffer, filled, buffer.size - filled)
            if (n == 0) break
            filled += n
          }
          buffer.copyOf(filled)
        }
      }
    }
    return parts.fold(ByteArray(0)) { all, part ->
      try {
        all + part.get()
      } catch (e: ExecutionException) {
        throw e.cause ?: e
      }
    }
  } finally {
    threads.shutdown()
  }
}
