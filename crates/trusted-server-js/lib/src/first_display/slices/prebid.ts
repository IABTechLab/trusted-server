import { installPrebidInitial } from '../leaf/prebid_protocol';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

registerCurrentFirstDisplayComponent('prebid_initial', installPrebidInitial);
