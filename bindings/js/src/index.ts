// Temple's TypeScript API, the same on React Native and the web. `wrap` builds it from the
// bindings uniffi-bindgen-react-native generates; the platform packages export the result.

export * from './api';
export { wrap, type Binding } from './wrap';
