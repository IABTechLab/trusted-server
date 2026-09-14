import { installApsInitial } from '../leaf/aps_protocol';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

registerCurrentFirstDisplayComponent('aps_initial', installApsInitial);
