// Words for people: Temple's 12 indices as words of a list of 2048, and back.

export { english } from './english';

/** The words of 12 indices, from a list of 2048 such as `english`. */
export function toWords(indices: number[], list: readonly string[]): string[] {
  return indices.map((index) => list[index]);
}

/** The indices of typed words – case, spacing and how accents are composed don't matter – or null if one isn't on the list. */
export function toIndices(text: string, list: readonly string[]): number[] | null {
  // As BIP-39 compares words: decomposed (NFKD), so that "é" typed one way matches the list's other.
  const normal = (word: string) => word.normalize('NFKD').toLowerCase();
  const words = list.map(normal);
  const indices = normal(text).trim().split(/\s+/).filter(Boolean).map((word) => words.indexOf(word));
  return indices.includes(-1) ? null : indices;
}
