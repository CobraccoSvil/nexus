// Re-export default per next/dynamic — evita il pattern .then(mod => ({ default: mod.X }))
// che causa webpack error 'Cannot read properties of undefined (reading call)' con named exports
export { SidebarManager as default } from './sidebar-manager';
