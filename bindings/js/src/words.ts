// Words for people: Temple's 12 indices as words of a list of 2048, and back.

export { english } from './english';

/** The words of 12 indices, from a list of 2048 such as `english`. */
export function toWords(indices: number[], list: readonly string[]): string[] {
  return indices.map((index) => list[index]);
}

/** The indices of typed words – case and spacing don't matter – or null if one isn't on the list. */
export function toIndices(text: string, list: readonly string[]): number[] | null {
  const indices = text.trim().toLowerCase().split(/\s+/).filter(Boolean).map((word) => list.indexOf(word));
  return indices.includes(-1) ? null : indices;
}
