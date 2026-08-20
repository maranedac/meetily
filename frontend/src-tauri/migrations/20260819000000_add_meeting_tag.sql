-- Adds a single optional tag per meeting, used to group meetings in the sidebar's
-- "Meeting Notes" list (see Sidebar/SidebarProvider.tsx). Deliberately one flat
-- nullable column, not a many-to-many tags table - a meeting has at most one tag.
ALTER TABLE meetings ADD COLUMN tag TEXT;
