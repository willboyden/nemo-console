import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
// OpenShell: the gateway that contains Data Designer runs. Config files are read-only (ADR 0007).
const DESC = { system: 'openshell', charts: [] };
class NcOpenShell extends NcView {
  setup() { genericSystemView(this, DESC); }
}
customElements.define('nc-openshell', NcOpenShell);
