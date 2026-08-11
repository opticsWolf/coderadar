import { createBrowserRouter, Routes, Route, Link, NavLink } from 'react-router-dom';
import AppLayout from './layouts/AppLayout';
import HomePage from './pages/HomePage';
import UserList from './pages/UserList';
import UserDetail from './pages/UserDetail';
import Settings from './pages/Settings';

// v6 JSX style
function JsxRoutes() {
  return (
    <Routes>
      <Route path="/" element={<AppLayout />}>
        <Route index element={<HomePage />} />
        <Route path="users" element={<UserList />} />
        <Route path="users/:id" element={<UserDetail />} />
        <Route path="settings" element={<Settings />} />
      </Route>
    </Routes>
  );
}

// v6.4+ data router style
const dataRouter = createBrowserRouter([
  {
    path: '/',
    element: <AppLayout />,
    children: [
      { index: true, element: <HomePage /> },
      { path: 'users', element: <UserList /> },
      { path: 'users/:id', element: <UserDetail /> },
    ],
  },
]);

// Navigation links
function Nav() {
  return (
    <nav>
      <Link to="/users">Users</Link>
      <NavLink to="/settings">Settings</NavLink>
    </nav>
  );
}

export { JsxRoutes, dataRouter, Nav };
