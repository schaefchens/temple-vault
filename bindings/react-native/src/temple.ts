// Temple for React Native on iOS and Android. A path is an absolute file path in the app's
// own storage, such as expo-file-system's document directory. The app needs
// `@ubjs/react-native` as a direct dependency: it loads the native library.

import { wrap, type Temple } from '@schaefchens/temple';
import * as binding from './index';

export * from '@schaefchens/temple';

export const temple: Temple = wrap(binding);
