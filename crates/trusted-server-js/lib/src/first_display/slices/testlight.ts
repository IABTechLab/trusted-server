import { installTestlightInitial } from '../leaf/callback_capture';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

registerCurrentFirstDisplayComponent('testlight_initial', installTestlightInitial);
