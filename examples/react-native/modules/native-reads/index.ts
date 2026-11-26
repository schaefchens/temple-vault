// The demo's own native module, which reads a space through TempleReader as a media player
// would: for Temple's self-test. Swift in ios/, Kotlin in android/.

import { TempleError, type ErrorKind } from '@schaefchens/temple-react-native';
import type { NativeReads } from '@schaefchens/temple/self-test';
import { requireNativeModule } from 'expo';

const module = requireNativeModule<NativeReads>('NativeReads');

/** Its errors as TempleError, of the kind native code got. */
export const nativeReads: NativeReads = {
  objectLength: (handle, id) => kind(module.objectLength(handle, id)),
  readObject: (handle, id, start, end) => kind(module.readObject(handle, id, start, end)),
};

async function kind<T>(call: Promise<T>): Promise<T> {
  try {
    return await call;
  } catch (error) {
    const { code, message } = error as { code?: string; message?: string };
    throw new TempleError(code as ErrorKind, message ?? String(error));
  }
}
