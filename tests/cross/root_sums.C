// ROOT's own reading of every tree of a file, condensed: for every branch
// holding numbers, the number of values and their sum over all entries,
// read the two ways ROOT reads data: entry by entry (`GetEntry`, then the
// leaf's values) and through `TTree::Draw` (arrays and collections
// flattened, a collection's own branch its sizes). Each has blind spots
// (Draw misreads baskets without offsets and reads char arrays as
// strings; the leaf API misreads STL members of classes without a
// dictionary), so a reading that matches either is ROOT's. The
// coverage measure (`tests/coverage.rs`) compares
// rhizoid's reading of the corpus with this, branch by branch;
// `tests/cases` take their expected values from it.
//
//   root -b -q -l 'tests/cross/root_sums.C("file.root")'
//   root -b -q -l 'tests/cross/root_sums.C("file.root", 17)'   # from branch 17 on
//   root -b -q -l 'tests/cross/root_sums.C("file.root", 0, 5, 12)'   # entries [5, 12)
//
// Lines: `TREE path entries`; for each branch `BEGIN i tree branch`, then
// `DRAW tree branch count sum` (if Draw reads it) and `SUM tree branch
// count sum` (entry by entry), or `SKIP tree branch reason` (strings,
// objects, several leaves). ROOT itself crashes on
// some branches of classes without a dictionary: the caller restarts after
// the branch whose BEGIN has no result.

#include <TBranch.h>
#include <TBranchElement.h>
#include <TDirectory.h>
#include <TFile.h>
#include <TKey.h>
#include <TLeaf.h>
#include <TSystem.h>
#include <TTree.h>

#include <algorithm>
#include <cstdio>
#include <string>

static int gIndex = 0;
static int gFrom = 0;
static Long64_t gFirst = 0;
static Long64_t gStop = -1;

static bool number_type(std::string t)
{
   static const char *names[] = {"Bool_t", "Char_t", "UChar_t", "Short_t", "UShort_t", "Int_t", "UInt_t",
                                 "Long64_t", "ULong64_t", "Float_t", "Double_t", "Long_t", "ULong_t",
                                 "Double32_t", "Float16_t", "bool", "char", "short", "int", "long",
                                 "float", "double", "unsigned char", "unsigned short", "unsigned int",
                                 "unsigned long", "long long", "unsigned long long"};
   for (auto n : names)
      if (t == n)
         return true;
   return false;
}

static bool numeric(TLeaf *leaf)
{
   std::string t = leaf->GetTypeName();
   if (number_type(t))
      return true;
   // std::vector of numbers.
   if (t.rfind("vector<", 0) == 0 && t.back() == '>')
      return number_type(t.substr(7, t.size() - 8));
   return false;
}

static void branches(TTree *t, const std::string &path, TObjArray *list)
{
   for (auto o : *list) {
      auto b = static_cast<TBranch *>(o);
      auto be = dynamic_cast<TBranchElement *>(b);
      bool counter = be && (be->GetType() == 3 || be->GetType() == 4);
      Long64_t first = std::min(gFirst, t->GetEntries());
      Long64_t stop = gStop < 0 ? t->GetEntries() : std::max(first, std::min(gStop, t->GetEntries()));
      if (b->GetListOfBranches()->GetEntriesFast() > 0) {
         branches(t, path, b->GetListOfBranches());
         // A split collection's own branch is its counter.
         if (!counter)
            continue;
      }
      int index = gIndex++;
      if (index < gFrom)
         continue;
      printf("BEGIN %d %s %s\n", index, path.c_str(), b->GetName());
      fflush(stdout);
      auto leaves = b->GetListOfLeaves();
      if (leaves->GetEntriesFast() != 1) {
         printf("SKIP %s %s leaves=%d\n", path.c_str(), b->GetName(), leaves->GetEntriesFast());
         continue;
      }
      auto leaf = static_cast<TLeaf *>(leaves->At(0));
      if (std::string(leaf->ClassName()) == "TLeafC") {
         printf("SKIP %s %s type=string\n", path.c_str(), b->GetName());
         continue;
      }
      if (!counter && !numeric(leaf)) {
         printf("SKIP %s %s type=%s\n", path.c_str(), b->GetName(), leaf->GetTypeName());
         continue;
      }
      // ROOT's second way of reading: TTree::Draw (arrays flattened). Draw
      // reads an array of chars as a string, one value per entry: not a
      // reading of the numbers.
      std::string ltype = leaf->GetTypeName();
      bool chars = (ltype == "Char_t" || ltype == "UChar_t" || ltype == "char" || ltype == "unsigned char") &&
                   (leaf->GetLeafCount() || leaf->GetLenStatic() > 1);
      if (!chars) {
         std::string name = b->GetName();
         std::string expr = name.substr(0, name.find('['));
         if (counter)
            expr = "@" + expr + ".size()";
         t->SetEstimate(t->GetEntries() + 1);
         Long64_t d = t->Draw(expr.c_str(), "", "goff", stop - first, first);
         if (d >= 0 && t->GetSelectedRows() >= t->GetEstimate()) {
            t->SetEstimate(t->GetSelectedRows() + 1);
            d = t->Draw(expr.c_str(), "", "goff", stop - first, first);
         }
         if (d >= 0) {
            double dsum = 0;
            for (Long64_t i = 0; i < t->GetSelectedRows(); ++i)
               dsum += t->GetV1()[i];
            printf("DRAW %s %s %lld %.17g\n", path.c_str(), b->GetName(), t->GetSelectedRows(), dsum);
            fflush(stdout);
         }
      }
      // Entry by entry, as ROOT reads data (GetEntry, then the leaf's
      // values), not through TTree::Draw, which misreads some baskets.
      auto count = leaf->GetLeafCount();
      Long64_t n = 0;
      double sum = 0;
      for (Long64_t i = first; i < stop; ++i) {
         if (count)
            count->GetBranch()->GetEntry(i);
         if (b->GetEntry(i) < 0) {
            n = -1;
            break;
         }
         if (counter) {
            n += 1;
            sum += be->GetNdata();
            continue;
         }
         int len = leaf->GetLen();
         // A negative length: the leaf API cannot read the member without
         // the class's dictionary.
         if (len < 0) {
            n = -1;
            break;
         }
         for (int j = 0; j < len; ++j)
            sum += leaf->GetValue(j);
         n += len;
      }
      if (n < 0) {
         printf("NOREAD %s %s\n", path.c_str(), b->GetName());
         continue;
      }
      printf("SUM %s %s %lld %.17g\n", path.c_str(), b->GetName(), n, sum);
      fflush(stdout);
   }
}

static void walk(TDirectory *d, const std::string &prefix)
{
   for (auto k : *d->GetListOfKeys()) {
      auto key = static_cast<TKey *>(k);
      std::string name = prefix.empty() ? key->GetName() : prefix + "/" + key->GetName();
      std::string cls = key->GetClassName();
      if (cls == "TDirectoryFile" || cls == "TDirectory") {
         walk(static_cast<TDirectory *>(key->ReadObj()), name);
      } else if (cls == "TTree" || cls == "TNtuple" || cls == "TNtupleD") {
         // The highest cycle only, as readers open a name.
         if (d->GetKey(key->GetName()) != key)
            continue;
         auto t = static_cast<TTree *>(key->ReadObj());
         printf("TREE %s %lld\n", name.c_str(), t->GetEntries());
         branches(t, name, t->GetListOfBranches());
      }
   }
}

void root_sums(const char *path, int from = 0, Long64_t first = 0, Long64_t stop = -1)
{
   setvbuf(stdout, nullptr, _IOLBF, 0);
   gFrom = from;
   gFirst = first;
   gStop = stop;
   TFile *f = TFile::Open(path);
   if (!f || f->IsZombie()) {
      printf("FILE error cannot-open\n");
      gSystem->Exit(1);
   }
   walk(f, "");
   printf("DONE\n");
}
