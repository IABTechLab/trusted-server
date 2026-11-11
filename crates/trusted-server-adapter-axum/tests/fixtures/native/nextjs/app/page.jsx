import { acceptMessage } from './actions'

export default function Page() {
  return (
    <main>
      <h1>Native real Next.js acceptance</h1>
      <form action={acceptMessage}>
        <input name="message" defaultValue="native-acceptance" />
        <button type="submit">Submit</button>
      </form>
    </main>
  )
}
