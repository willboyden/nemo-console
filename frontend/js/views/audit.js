import { NcView } from '../components/base.js';
import { t } from '../i18n.js';
import { consolePage } from '../components/system-page.js';
import { summaryTab } from '../audit/summary-tab.js';
import { logTab } from '../audit/log-tab.js';

// Audit (admin only): since addendum 9.16 a Summary tab (actions per day stacked by system, an actor x system matrix, the error timeline) before the Log, whose time-range, actor, status and
// system filters live in the URL hash.
class NcAudit extends NcView {
  setup() {
    consolePage(this, {
      page: 'audit',
      active: this.query?.tab,
      tabDefs: [
        { id: 'summary', label: t('tab.summary'), render: (scope, ctx) => summaryTab(scope, ctx) },
        { id: 'log', label: t('tab.log'), render: (scope, ctx) => logTab(scope, ctx) },
      ],
    });
  }
}
customElements.define('nc-audit', NcAudit);
