import { log } from '../core/log';


export function installPermutiveShim() {
  //@ts-ignore
  permutive.config.apiHost = window.location.host + "/permutive/api";
}

