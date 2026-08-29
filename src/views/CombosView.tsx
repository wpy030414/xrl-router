import { useState, useEffect } from 'react';
import { useNavigate } from 'react-router';
import { Plus, Inbox, MoreVertical, Pencil, Trash2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { useCombosStore } from '@/stores/combos';
import { combosApi, type Combo } from '@/lib/api';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';

interface ComboCardProps {
  combo: Combo;
  onEdit: () => void;
  onDelete: () => void;
}

function ComboCard({ combo, onEdit, onDelete }: ComboCardProps) {
  const t = useT();

  return (
    <article className="bg-muted rounded-lg p-5 grid grid-cols-[1fr_auto] gap-3 items-start cursor-default">
      <div className="flex flex-col gap-0.5 min-w-0">
        <div className="flex items-center gap-2">
          <h3 className="font-medium truncate" title={combo.name}>
            {combo.name}
          </h3>
          {!combo.enabled && (
            <span className="inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium bg-muted-foreground/20 text-muted-foreground">
              {t('combos.disabled')}
            </span>
          )}
        </div>
        <div className="flex flex-wrap gap-1 mt-2">
          {combo.members.map((member, idx) => (
            <span
              key={idx}
              className="inline-flex items-center gap-1 px-2 py-1 rounded-md bg-background border text-xs"
              title={t('combos.member_order', { pos: idx + 1 })}
            >
              <span className="font-mono">{member}</span>
            </span>
          ))}
        </div>
      </div>

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon" className="w-9 h-9">
            <MoreVertical className="w-5 h-5" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onClick={onEdit}>
            <Pencil className="w-4 h-4 mr-2" />
            {t('common.edit')}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={onDelete} className="text-destructive focus:text-destructive">
            <Trash2 className="w-4 h-4 mr-2" />
            {t('common.delete')}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </article>
  );
}

export function CombosView() {
  const t = useT();
  const navigate = useNavigate();
  const { combos, fetchCombos } = useCombosStore();

  const [loading, setLoading] = useState(true);
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<Combo | null>(null);

  // Load data
  useEffect(() => {
    const load = async () => {
      setLoading(true);
      try {
        await fetchCombos();
      } finally {
        setLoading(false);
      }
    };
    load();
  }, []);

  const handleEdit = (combo: Combo) => {
    navigate(`/combos/${combo.id}/edit`);
  };

  const handleDelete = (combo: Combo) => {
    setDeleteTarget(combo);
    setDeleteDialogOpen(true);
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    await combosApi.delete(deleteTarget.id);
    setDeleteDialogOpen(false);
    setDeleteTarget(null);
    await fetchCombos();
  };

  return (
    <div className="space-y-6">
      <div className="flex justify-between items-start gap-4 flex-wrap">
        <h2 className="text-3xl font-normal m-0">{t('combos.title')}</h2>
        <Button onClick={() => navigate('/combos/new')}>
          <Plus className="w-4 h-4 mr-2" />
          {t('combos.create')}
        </Button>
      </div>

      {loading ? (
        <div className="flex flex-col items-center justify-center py-16">
          <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
        </div>
      ) : combos.length === 0 ? (
        <div className="flex flex-col items-center gap-2 py-16 text-center">
          <Inbox className="w-12 h-12 text-muted-foreground" />
          <p className="text-lg">{t('common.empty')}</p>
        </div>
      ) : (
        <div className="grid grid-cols-[repeat(auto-fill,minmax(320px,1fr))] gap-4">
          {combos.map((combo) => (
            <ComboCard
              key={combo.id}
              combo={combo}
              onEdit={() => handleEdit(combo)}
              onDelete={() => handleDelete(combo)}
            />
          ))}
        </div>
      )}

      <Dialog open={deleteDialogOpen} onOpenChange={setDeleteDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('combos.delete_title')}</DialogTitle>
            <DialogDescription>
              {t('combos.delete_confirm', { name: deleteTarget?.name || '' })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteDialogOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button variant="destructive" onClick={confirmDelete}>
              {t('combos.delete_confirm_btn')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

export default CombosView;
