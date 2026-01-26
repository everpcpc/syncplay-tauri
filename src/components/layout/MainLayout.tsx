export function MainLayout() {
  return (
    <div className="flex flex-col h-screen bg-gray-900 text-white">
      <header className="bg-gray-800 p-4 border-b border-gray-700">
        <h1 className="text-xl font-bold">Syncplay</h1>
      </header>

      <div className="flex flex-1 overflow-hidden">
        <aside className="w-64 bg-gray-800 border-r border-gray-700 p-4">
          <h2 className="text-lg font-semibold mb-4">Users</h2>
          <p className="text-gray-400 text-sm">Not connected</p>
        </aside>

        <main className="flex-1 flex flex-col">
          <div className="flex-1 p-4 overflow-auto">
            <p className="text-gray-400">
              Welcome to Syncplay! Connect to a server to get started.
            </p>
          </div>

          <div className="border-t border-gray-700 p-4">
            <input
              type="text"
              placeholder="Type a message..."
              className="w-full bg-gray-800 text-white px-4 py-2 rounded border border-gray-700 focus:outline-none focus:border-blue-500"
              disabled
            />
          </div>
        </main>
      </div>
    </div>
  );
}
