/** Feature-owned module demand; a failed native import can remain rejected until document reload. */
export const loadCalendarGrid = () => import('./calendar-view.tsx');
