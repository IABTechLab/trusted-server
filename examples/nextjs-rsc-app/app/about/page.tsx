// About page — produces RSC payloads with HTML content (T-chunks).
// The RSC Flight data will include rendered HTML containing URLs,
// which exercises the T-chunk rewriting path.
const ORIGIN = "https://origin.example.com:3099";

interface TeamMember {
  name: string;
  role: string;
  profileUrl: string;
  avatarUrl: string;
}

const team: TeamMember[] = [
  {
    name: "Alice Johnson",
    role: "Engineering Lead",
    profileUrl: `${ORIGIN}/team/alice`,
    avatarUrl: `${ORIGIN}/avatars/alice.jpg`,
  },
  {
    name: "Bob Smith",
    role: "Product Manager",
    profileUrl: `${ORIGIN}/team/bob`,
    avatarUrl: `${ORIGIN}/avatars/bob.jpg`,
  },
  {
    name: "Carol Williams",
    role: "Designer",
    profileUrl: `${ORIGIN}/team/carol`,
    avatarUrl: `${ORIGIN}/avatars/carol.jpg`,
  },
];

export default function AboutPage() {
  return (
    <div>
      <h1>About Us</h1>
      <p>
        We are building at{" "}
        <a href={`${ORIGIN}/about`}>origin.example.com</a>.
      </p>

      <section>
        <h2>Our Team</h2>
        {team.map((member) => (
          <div key={member.name}>
            <img src={member.avatarUrl} alt={member.name} width={64} height={64} />
            <h3>
              <a href={member.profileUrl}>{member.name}</a>
            </h3>
            <p>{member.role}</p>
          </div>
        ))}
      </section>

      <section>
        <h2>Resources</h2>
        <ul>
          <li>
            <a href={`${ORIGIN}/blog`}>Blog</a>
          </li>
          <li>
            <a href={`${ORIGIN}/careers`}>Careers</a>
          </li>
          <li>
            <a href={`${ORIGIN}/contact`}>Contact</a>
          </li>
        </ul>
      </section>
    </div>
  );
}
