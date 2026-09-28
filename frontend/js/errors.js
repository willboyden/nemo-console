// Human-readable, actionable text for API failures. Contract errors are {error:{code,message}}.
import { t } from './i18n.js';

/** Status -> i18n key. The message from the server is appended when it adds information. */
const STATUS_KEY = { 401: 'err.401', 403: 'err.403', 409: 'err.409', 413: 'err.413', 415: 'err.415', 421: 'err.421', 429: 'err.429', 503: 'err.503', 502: 'err.502' };

export function describeError(e) {
  if (!e) return t('common.error');
  const status = e.status, detail = e.message;
  if (status === 0 || e.code === 'network') return t('err.network');
  const key = STATUS_KEY[status];
  if (!key) return detail || t('common.error');
  const base = t(key);
  // The server's own words are kept when they say something the generic advice does not.
  return detail && !base.includes(detail) ? `${base} (${detail})` : base;
}
