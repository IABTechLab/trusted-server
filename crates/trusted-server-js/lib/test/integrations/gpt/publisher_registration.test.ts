import { publisherRegistrationContract } from '../../fixtures/publisher_registration_contract';

for (const implementation of [
  'runtime',
  'bootstrap',
  'waiting_handoff',
  'requested_handoff',
] as const) {
  publisherRegistrationContract(implementation);
}
