export default function Contact() {
  const originHost = process.env.ORIGIN_HOST || "127.0.0.1:8888";

  return (
    <main>
      <h1>Contact Us</h1>
      <p>Send us a message using the form below.</p>

      {/* Form with action URL pointing to origin — the proxy should rewrite it. */}
      <form id="contact-form" action={`http://${originHost}/api/contact`} method="POST">
        <label htmlFor="name">Name</label>
        <input type="text" id="name" name="name" required />

        <label htmlFor="email">Email</label>
        <input type="email" id="email" name="email" required />

        <label htmlFor="message">Message</label>
        <textarea id="message" name="message" required rows={5} />

        <button type="submit">Send Message</button>
      </form>

      <div id="ad-slot-contact" data-ad-unit="/test/contact-sidebar">
        <a href={`http://${originHost}/ad/contact-landing`}>Contact ad</a>
      </div>
    </main>
  );
}
