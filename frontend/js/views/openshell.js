import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
import { openshellExtras } from '../openshell/extras.js';
// OpenShell: the gateway that contains Data Designer runs. Config files are read-only (ADR 0007). Since addendum 9.16, on Status: sandbox lifecycle swim lanes (from the persisted
// `openshell.sandbox` events), the sandbox count by phase over time, and the policy decisions of one sandbox (read-only: no control here changes a policy).
const DESC = {
  system: 'openshell',
  charts: [], // the phase counts are the stacked area below (a tile and the all-values table keep the numbers)
  extras: (scope, shared) => openshellExtras(scope, shared).el,
};
class NcOpenShell extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-openshell', NcOpenShell);
