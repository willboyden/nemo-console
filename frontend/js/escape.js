// The single escaping primitive used before any markup is generated (charts.js). Everything that reaches
// setTrustedHtml must have passed through here; everything else uses textContent via dom.js h().
export function escapeHtml(s) {
  return String(s ?? '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}
