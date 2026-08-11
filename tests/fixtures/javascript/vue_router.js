import { createRouter, createWebHistory } from 'vue-router';
import HomeView from '@/views/HomeView.vue';
import UserList from '@/views/UserList.vue';
import UserDetail from '@/views/UserDetail.vue';
import Settings from '@/views/Settings.vue';
import AdminLayout from '@/layouts/AdminLayout.vue';

const routes = [
  {
    path: '/',
    name: 'home',
    component: HomeView,
  },
  {
    path: '/users',
    name: 'users',
    component: UserList,
  },
  {
    path: '/users/:id',
    name: 'user-detail',
    component: UserDetail,
  },
  {
    path: '/settings',
    name: 'settings',
    component: () => import('@/views/Settings.vue'),
  },
];

const router = createRouter({
  history: createWebHistory(),
  routes,
});

// Dynamic route addition
router.addRoute('admin', {
  path: '/dashboard',
  component: () => import('@/layouts/AdminLayout.vue'),
});

export default router;
