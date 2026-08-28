import { installCreativeInitial } from '../leaf/creative_guard';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

registerCurrentFirstDisplayComponent('creative_initial', installCreativeInitial);
