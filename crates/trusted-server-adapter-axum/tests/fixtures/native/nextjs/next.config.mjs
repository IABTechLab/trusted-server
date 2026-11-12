// The local origin explicitly trusts only this fixture publisher for actions.
export default {
  experimental: {
    serverActions: { allowedOrigins: ['publisher.example.com'] },
  },
}
