import { currentLocale } from "./i18n.js";

const TAGS = { en: "en-US", ja: "ja-JP" };
const formats = new Map();

function format(options) {
  const tag = TAGS[currentLocale()] ?? TAGS.en, key = `${tag} ${JSON.stringify(options)}`;
  if (!formats.has(key)) formats.set(key, new Intl.NumberFormat(tag, options));
  return formats.get(key);
}

export const count = (n) => format({}).format(Number(n));

/** Formats `n` with exactly `digits` decimal places. */
export const fixed = (n, digits) => format({ minimumFractionDigits: digits, maximumFractionDigits: digits }).format(n);

/** Formats `n` with at most three significant digits. */
export const significant = (n) => format({ maximumSignificantDigits: 3 }).format(n);

/** Returns 0–2 decimal places based on the smallest positive value in `xs`, or 0 if none exists. */
export function places(xs) {
  const min = Math.min(...xs.filter((x) => x > 0));
  return Number.isFinite(min) ? Math.max(0, Math.min(2, Math.ceil(-Math.log10(min)) + 1)) : 0;
}
