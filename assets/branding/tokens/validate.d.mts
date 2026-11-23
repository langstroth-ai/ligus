// Types for validate.mjs, for the web tests.
export const TOKENS_PATH: string;
export const STATUS_MIN_DELTA_E: number;
export const WARN_WARM_MIN_DELTA_E: number;
export interface TokenCheck {
  id: string;
  pass: boolean;
  detail: string;
}
export function validate(tokens: unknown): { ok: boolean; failures: string[]; checks: TokenCheck[] };
