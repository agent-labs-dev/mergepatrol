/* PROTOTYPE, throw away. The PR detail variants, apart from their chunk. */
export const VARIANTS = {
  A: "Tabs, like T3 Code",
  B: "Readiness, one scroll",
  C: "Summary, expand to review",
} as const;
export type Variant = keyof typeof VARIANTS;
