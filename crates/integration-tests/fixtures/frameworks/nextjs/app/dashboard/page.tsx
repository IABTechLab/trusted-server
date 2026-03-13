import RouteScript from "../components/RouteScript";

export default function Dashboard() {
  const originHost = process.env.ORIGIN_HOST || "127.0.0.1:8888";

  const stats = [
    { label: "Total Users", value: "1,234" },
    { label: "Active Sessions", value: "56" },
    { label: "API Calls", value: "12,345" },
    { label: "Success Rate", value: "99.9%" },
  ];

  return (
    <main>
      <h1>Dashboard</h1>
      <p>Sample dashboard page for testing script injection on content-heavy pages.</p>

      <ul id="stats-list">
        {stats.map((stat) => (
          <li key={stat.label}>
            <strong>{stat.label}:</strong> {stat.value}
          </li>
        ))}
      </ul>

      <div id="ad-slot-dashboard" data-ad-unit="/test/dashboard-leaderboard">
        <a href={`http://${originHost}/ad/dashboard-landing`}>Dashboard ad</a>
        <img src={`http://${originHost}/ad/leaderboard.png`} alt="leaderboard" />
      </div>

      <RouteScript marker="dashboard" />
    </main>
  );
}
