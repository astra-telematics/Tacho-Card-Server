import type { RouteRecordRaw } from 'vue-router'

const routes: RouteRecordRaw[] = [
    {
        path: '/',
        component: () => import('layouts/MainLayout.vue'),
        children: [
            { path: '', component: () => import('pages/IndexPage.vue') },
            { path: 'companies', component: () => import('pages/CompaniesPage.vue') },
            { path: 'devices', component: () => import('pages/DevicesPage.vue') },
            { path: 'users', component: () => import('pages/UsersPage.vue') },
        ],
    },

    // Web UI only (headless server); the desktop app is always logged in as admin.
    { path: '/login', component: () => import('pages/LoginPage.vue') },

    // Always leave this as last one,
    // but you can also remove it
    {
        path: '/:catchAll(.*)*',
        component: () => import('pages/ErrorNotFound.vue'),
    },
]

export default routes
